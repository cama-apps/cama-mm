"""
End-to-end tests for jopacoin betting in match recording.
"""

import pytest

from config import JOPACOIN_EXCLUSION_REWARD, JOPACOIN_WIN_REWARD
from database import Database
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService


class TestBettingEndToEnd:
    """End-to-end coverage for jopacoin wagers."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create test players in the database."""
        player_ids = [1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return player_ids

    def test_bets_settle_with_house(self, test_db, test_players):
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        player_ids = test_players[:10]
        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)
        participant = pending["radiant_team_ids"][0]
        spectator = 9000
        test_db.add_player(
            discord_id=spectator,
            discord_username="Spectator",
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        player_repo.add_balance(participant, 20)
        player_repo.add_balance(spectator, 10)

        betting_service.place_bet(1, participant, "radiant", 5, pending)
        betting_service.place_bet(1, spectator, "dire", 5, pending)

        result = match_service.record_match("radiant", guild_id=1)

        assert "bet_distributions" in result
        distributions = result["bet_distributions"]
        assert distributions["winners"], "Expected at least one winning distribution"
        assert distributions["winners"][0]["discord_id"] == participant
        assert distributions["losers"][0]["discord_id"] == spectator

        expected_participant_balance = 3 + 20 - 5 + 10 + JOPACOIN_WIN_REWARD
        assert player_repo.get_balance(participant) == expected_participant_balance
        # Spectator starts with 3, gets +10 top-up, -5 lost bet = 8
        assert player_repo.get_balance(spectator) == 8

    def test_excluded_players_receive_exclusion_bonus(self, test_db):
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        player_ids = list(range(5101, 5113))  # 12 players -> 2 excluded
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)
        excluded_ids = pending["excluded_player_ids"]
        assert len(excluded_ids) == 2

        # Start everyone at zero for deterministic balance checks
        for pid in player_ids:
            player_repo.update_balance(pid, 0)

        match_service.record_match("radiant", guild_id=1)

        for pid in excluded_ids:
            assert player_repo.get_balance(pid) == JOPACOIN_EXCLUSION_REWARD

        included_ids = set(player_ids) - set(excluded_ids)
        for pid in included_ids:
            assert player_repo.get_balance(pid) != JOPACOIN_EXCLUSION_REWARD

    def test_max_lobby_14_players_4_excluded(self, test_db):
        """
        Test max lobby scenario: 14 players with 4 excluded.

        This tests the maximum lobby size to ensure all excluded players
        are correctly tracked and receive exclusion bonuses.
        """
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=True,
        )

        # Mix of positive IDs (real users) and negative IDs (fake users)
        real_ids = [6001, 6002, 6003, 6004]
        fake_ids = list(range(-1, -11, -1))  # -1 through -10
        player_ids = real_ids + fake_ids  # 14 total

        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{abs(pid)}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
                preferred_roles=["1", "2", "3", "4", "5"],
            )

        result = match_service.shuffle_players(player_ids, guild_id=1)
        excluded_ids = result["excluded_ids"]

        # With 14 players and 10 selected, exactly 4 must be excluded
        assert len(excluded_ids) == 4, f"Expected 4 excluded, got {len(excluded_ids)}"

        # All excluded IDs must be valid player IDs
        for pid in excluded_ids:
            assert pid in player_ids, f"Excluded ID {pid} not in player_ids"
            player = player_repo.get_by_id(pid)
            assert player is not None, f"Player {pid} not found in repo"

        # Verify exclusion IDs are stored in pending state
        pending = match_service.get_last_shuffle(1)
        assert pending["excluded_player_ids"] == excluded_ids

        # Verify radiant + dire + excluded = all players
        all_in_match = set(pending["radiant_team_ids"] + pending["dire_team_ids"])
        all_excluded = set(excluded_ids)
        assert len(all_in_match) == 10
        assert len(all_excluded) == 4
        assert all_in_match.isdisjoint(all_excluded), "Excluded player found in match teams"
        assert all_in_match | all_excluded == set(player_ids)

    def test_betting_totals_display_correctly_after_previous_match(self, test_db, test_players):
        """
        E2E test for the betting totals display bug fix.

        Scenario: User bets 6 jopacoin on Dire, but it shows as 3 because
        previous settled bets are being counted. This test verifies the fix.
        """
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        # First match: Create and settle with some bets
        player_ids_match1 = test_players[:10]
        match_service.shuffle_players(player_ids_match1, guild_id=1)
        pending1 = match_service.get_last_shuffle(1)

        spectator1 = 9001
        spectator2 = 9002
        test_db.add_player(
            discord_id=spectator1,
            discord_username="Spectator1",
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        test_db.add_player(
            discord_id=spectator2,
            discord_username="Spectator2",
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        player_repo.add_balance(spectator1, 20)
        player_repo.add_balance(spectator2, 20)

        # Place bets on first match: 3 on radiant, 2 on dire
        betting_service.place_bet(1, spectator1, "radiant", 3, pending1)
        betting_service.place_bet(1, spectator2, "dire", 2, pending1)

        # Verify totals show pending bets correctly
        totals = betting_service.get_pot_odds(1, pending_state=pending1)
        assert totals["radiant"] == 3, "Should show 3 jopacoin on Radiant"
        assert totals["dire"] == 2, "Should show 2 jopacoin on Dire"

        # Settle the first match (this assigns match_id to the bets)
        match_service.record_match("radiant", guild_id=1)

        # After settling, totals should be 0 (no pending bets)
        totals = betting_service.get_pot_odds(1, pending_state=pending1)
        assert totals["radiant"] == 0, "Should show 0 after settling (no pending bets)"
        assert totals["dire"] == 0, "Should show 0 after settling (no pending bets)"

        # Second match: Create new match and place new bets
        player_ids_match2 = [2001, 2002, 2003, 2004, 2005, 2006, 2007, 2008, 2009, 2010]
        for pid in player_ids_match2:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        match_service.shuffle_players(player_ids_match2, guild_id=1)
        pending2 = match_service.get_last_shuffle(1)

        # User bets 6 jopacoin on Dire (the exact bug scenario)
        spectator3 = 9003
        test_db.add_player(
            discord_id=spectator3,
            discord_username="Spectator3",
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.add_balance(spectator3, 20)

        betting_service.place_bet(1, spectator3, "dire", 6, pending2)

        # CRITICAL: Verify totals only show the new pending bet (6), not old settled bets
        # Before the fix, this would show 3 (6 - 3 from previous match, or some incorrect calculation)
        totals = betting_service.get_pot_odds(1, pending_state=pending2)
        assert totals["radiant"] == 0, "Should show 0 on Radiant (no pending bets)"
        assert totals["dire"] == 6, (
            f"Should show 6 jopacoin on Dire (the bet just placed), got {totals['dire']}"
        )

        # Verify the bet was recorded correctly
        bet = bet_repo.get_player_pending_bet(1, spectator3, since_ts=pending2["shuffle_timestamp"])
        assert bet is not None, "Bet should exist"
        assert bet["amount"] == 6, "Bet amount should be 6"
        assert bet["team_bet_on"] == "dire", "Bet should be on Dire"

    def test_betting_totals_multiple_bets_same_match(self, test_db, test_players):
        """
        E2E test: Multiple users place bets on the same match, verify totals are correct.
        """
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        player_ids = test_players[:10]
        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)

        # Create spectators
        spectators = []
        for i in range(4):
            spectator_id = 9100 + i
            test_db.add_player(
                discord_id=spectator_id,
                discord_username=f"Spectator{i}",
                initial_mmr=1100,
                glicko_rating=1100.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_repo.add_balance(spectator_id, 20)
            spectators.append(spectator_id)

        # Place multiple bets
        betting_service.place_bet(1, spectators[0], "radiant", 5, pending)
        betting_service.place_bet(1, spectators[1], "radiant", 3, pending)
        betting_service.place_bet(1, spectators[2], "dire", 4, pending)
        betting_service.place_bet(1, spectators[3], "dire", 6, pending)

        # Verify totals are correct
        totals = betting_service.get_pot_odds(1, pending_state=pending)
        assert totals["radiant"] == 8, (
            f"Should show 8 jopacoin on Radiant (5+3), got {totals['radiant']}"
        )
        assert totals["dire"] == 10, f"Should show 10 jopacoin on Dire (4+6), got {totals['dire']}"


class TestLoanRepaymentOnMatchRecord:
    """End-to-end tests for loan repayment when matches are recorded."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def services(self, test_db):
        """Create all required services with loan integration."""
        from services.loan_service import LoanRepository, LoanService

        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        loan_repo = LoanRepository(test_db.db_path)

        betting_service = BettingService(bet_repo, player_repo)
        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
        )
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
            loan_service=loan_service,
        )

        return {
            "player_repo": player_repo,
            "bet_repo": bet_repo,
            "match_repo": match_repo,
            "loan_repo": loan_repo,
            "betting_service": betting_service,
            "loan_service": loan_service,
            "match_service": match_service,
            "db": test_db,
        }

    @pytest.fixture
    def test_players(self, services):
        """Create 10 test players."""
        db = services["db"]
        player_ids = [3001, 3002, 3003, 3004, 3005, 3006, 3007, 3008, 3009, 3010]
        for pid in player_ids:
            db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return player_ids

    def test_loan_repaid_when_borrower_wins(self, services, test_players):
        """Loan is repaid when borrower participates in a match (winning team)."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        # Borrower starts with 0 balance (after default 3)
        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, 0)

        # Take a loan of 50 (fee = 10, total owed = 60)
        result = loan_service.take_loan(borrower_id, 50)
        assert result["success"]
        assert player_repo.get_balance(borrower_id) == 50  # Got full loan amount

        # Verify outstanding loan exists
        state = loan_service.get_state(borrower_id)
        assert state.has_outstanding_loan
        assert state.outstanding_principal == 50
        assert state.outstanding_fee == 10

        # Shuffle and record match (borrower is on radiant, radiant wins)
        match_service.shuffle_players(test_players, guild_id=1)
        pending = match_service.get_last_shuffle(1)

        # Make sure borrower is on radiant for predictable test
        if borrower_id not in pending["radiant_team_ids"]:
            # Swap teams so borrower is on winning side
            pending["radiant_team_ids"], pending["dire_team_ids"] = (
                pending["dire_team_ids"],
                pending["radiant_team_ids"],
            )

        # Record radiant win
        result = match_service.record_match("radiant", guild_id=1)

        # Loan should be repaid
        state = loan_service.get_state(borrower_id)
        assert not state.has_outstanding_loan
        assert state.outstanding_principal == 0
        assert state.outstanding_fee == 0

        # Balance: started 0, +50 loan, -60 repayment, +win_reward = -10 + win_reward
        expected = 50 - 60 + JOPACOIN_WIN_REWARD
        assert player_repo.get_balance(borrower_id) == expected

    def test_loan_repaid_when_borrower_loses(self, services, test_players):
        """Loan is repaid even when borrower loses the match."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, 0)

        # Take loan of 50
        loan_service.take_loan(borrower_id, 50)
        assert player_repo.get_balance(borrower_id) == 50

        # Shuffle match
        match_service.shuffle_players(test_players, guild_id=1)
        pending = match_service.get_last_shuffle(1)

        # Make sure borrower is on dire (losing side)
        if borrower_id not in pending["dire_team_ids"]:
            pending["radiant_team_ids"], pending["dire_team_ids"] = (
                pending["dire_team_ids"],
                pending["radiant_team_ids"],
            )

        # Radiant wins (borrower loses)
        match_service.record_match("radiant", guild_id=1)

        # Loan still repaid
        state = loan_service.get_state(borrower_id)
        assert not state.has_outstanding_loan

        # Balance: 0 + 50 loan - 60 repayment + 1 participation = -9 (debt)
        assert player_repo.get_balance(borrower_id) == -9

    def test_loan_not_repaid_if_not_participant(self, services, test_players):
        """Loan is NOT repaid if borrower doesn't participate in the match."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]
        db = services["db"]

        # Create a non-participant who takes a loan
        spectator_id = 9999
        db.add_player(
            discord_id=spectator_id,
            discord_username="Spectator",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.update_balance(spectator_id, 0)

        # Spectator takes loan
        loan_service.take_loan(spectator_id, 50)
        assert player_repo.get_balance(spectator_id) == 50

        # Match is played by different players
        match_service.shuffle_players(test_players, guild_id=1)
        match_service.record_match("radiant", guild_id=1)

        # Spectator's loan should NOT be repaid
        state = loan_service.get_state(spectator_id)
        assert state.has_outstanding_loan
        assert state.outstanding_principal == 50
        assert player_repo.get_balance(spectator_id) == 50  # Unchanged

    def test_loan_with_spectator_bet(self, services, test_players):
        """Spectator with loan bets on the match and loan repaid next game."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        betting_service = services["betting_service"]
        match_service = services["match_service"]
        db = services["db"]

        # Create a spectator who takes a loan
        spectator_id = 9998
        db.add_player(
            discord_id=spectator_id,
            discord_username="Spectator",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.update_balance(spectator_id, 0)

        # Spectator takes loan of 100 (fee = 20)
        loan_service.take_loan(spectator_id, 100)
        assert player_repo.get_balance(spectator_id) == 100

        # Match 1: spectator bets but doesn't play (house mode for 1:1 payout)
        match_service.shuffle_players(test_players, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)

        # Spectator bets 30 on radiant
        betting_service.place_bet(1, spectator_id, "radiant", 30, pending)
        assert player_repo.get_balance(spectator_id) == 70  # 100 - 30

        # Record radiant win
        match_service.record_match("radiant", guild_id=1)

        # Spectator won the bet: +30 back + 30 winnings = +60
        # But loan NOT repaid (spectator didn't participate)
        state = loan_service.get_state(spectator_id)
        assert state.has_outstanding_loan
        assert player_repo.get_balance(spectator_id) == 130  # 70 + 60

        # Match 2: Now spectator plays and loan is repaid
        new_players = test_players[:9] + [spectator_id]  # Replace one player with spectator
        match_service.shuffle_players(new_players, guild_id=1, betting_mode="house")
        match_service.record_match("radiant", guild_id=1)

        # Loan repaid now
        state = loan_service.get_state(spectator_id)
        assert not state.has_outstanding_loan

        # Balance: 130 - 120 repayment + (1 if lost, 2 if won)
        balance = player_repo.get_balance(spectator_id)
        # Winners get +2 (win bonus), losers get +1 (participation)
        assert balance in [11, 12]  # 130 - 120 + 1 = 11, or 130 - 120 + 2 = 12

    def test_loan_repayment_pushes_into_debt(self, services, test_players):
        """Loan repayment can push player into debt when they've spent the money."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, 0)

        # Take max loan of 100 (fee = 20, total owed = 120)
        loan_service.take_loan(borrower_id, 100)
        assert player_repo.get_balance(borrower_id) == 100

        # "Spend" the loan money (set balance to 0)
        player_repo.update_balance(borrower_id, 0)

        # Play a match
        match_service.shuffle_players(test_players, guild_id=1)
        match_service.record_match("radiant", guild_id=1)

        # Loan repaid, player in debt
        state = loan_service.get_state(borrower_id)
        assert not state.has_outstanding_loan

        # Balance: 0 - 120 + (1 if lost, 2 if won)
        balance = player_repo.get_balance(borrower_id)
        assert balance in [-119, -118]  # Lost: -119, Won: -118

    def test_multiple_players_with_loans(self, services, test_players):
        """Multiple players have loans, all repaid after match."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        # Two players take loans
        borrower1 = test_players[0]
        borrower2 = test_players[5]

        player_repo.update_balance(borrower1, 0)
        player_repo.update_balance(borrower2, 0)

        loan_service.take_loan(borrower1, 50)  # owes 60
        loan_service.take_loan(borrower2, 100)  # owes 120

        # Shuffle and record
        match_service.shuffle_players(test_players, guild_id=1)
        match_service.record_match("radiant", guild_id=1)

        # Both loans repaid
        assert not loan_service.get_state(borrower1).has_outstanding_loan
        assert not loan_service.get_state(borrower2).has_outstanding_loan

        # Balances: loan - repayment + reward
        # Winners get +2 (JOPACOIN_WIN_REWARD), losers get +1 (participation)
        # Borrower1: 50 - 60 + (2 if won, 1 if lost)
        # Borrower2: 100 - 120 + (2 if won, 1 if lost)
        b1_balance = player_repo.get_balance(borrower1)
        b2_balance = player_repo.get_balance(borrower2)

        # If won: loan - repayment + win_reward
        # If lost: loan - repayment + participation
        assert b1_balance in [-9, -8]  # 50 - 60 + 1 = -9, or 50 - 60 + 2 = -8
        assert b2_balance in [-19, -18]  # 100 - 120 + 1 = -19, or 100 - 120 + 2 = -18

    def test_loan_repayment_fee_goes_to_nonprofit(self, services, test_players):
        """Verify the loan fee is added to nonprofit fund on repayment."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        loan_repo = services["loan_repo"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, 0)

        # Get nonprofit fund before
        nonprofit_before = loan_repo.get_nonprofit_fund(1)

        # Take loan of 100 (fee = 20)
        loan_service.take_loan(borrower_id, 100)

        # Fee not added yet (deferred)
        assert loan_repo.get_nonprofit_fund(1) == nonprofit_before

        # Play and record match
        match_service.shuffle_players(test_players, guild_id=1)
        match_service.record_match("radiant", guild_id=1)

        # Now fee should be in nonprofit fund
        nonprofit_after = loan_repo.get_nonprofit_fund(1)
        assert nonprofit_after == nonprofit_before + 20

    def test_loan_repayment_result_in_match_record(self, services, test_players):
        """Verify loan repayments are reported in match record result."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, 0)
        loan_service.take_loan(borrower_id, 75)  # owes 90

        match_service.shuffle_players(test_players, guild_id=1)
        result = match_service.record_match("radiant", guild_id=1)

        # Check loan_repayments in result
        assert "loan_repayments" in result
        repayments = result["loan_repayments"]
        assert len(repayments) == 1
        assert repayments[0]["player_id"] == borrower_id
        assert repayments[0]["principal"] == 75
        assert repayments[0]["fee"] == 15
        assert repayments[0]["total_repaid"] == 90


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
