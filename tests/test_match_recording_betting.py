"""
End-to-end tests for jopacoin betting in match recording.
"""

import pytest
import os
import tempfile
import time

from config import JOPACOIN_WIN_REWARD
from database import Database
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository
from services.betting_service import BettingService
from services.match_service import MatchService


class TestBettingEndToEnd:
    """End-to-end coverage for jopacoin wagers."""
    
    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix='.db')
        os.close(fd)
        db = Database(db_path)
        yield db
        # Close any open connections before cleanup
        try:
            import sqlite3
            sqlite3.connect(db_path).close()
        except:
            pass
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except PermissionError:
            time.sleep(0.2)
            try:
                os.unlink(db_path)
            except:
                pass
    
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
                glicko_volatility=0.06
            )
        return player_ids

    def test_bets_settle_with_house(self, test_db, test_players):
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=False, betting_service=betting_service)

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
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=False, betting_service=betting_service)

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
                glicko_volatility=0.06
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
        assert totals["dire"] == 6, f"Should show 6 jopacoin on Dire (the bet just placed), got {totals['dire']}"
        
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
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=False, betting_service=betting_service)

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
        assert totals["radiant"] == 8, f"Should show 8 jopacoin on Radiant (5+3), got {totals['radiant']}"
        assert totals["dire"] == 10, f"Should show 10 jopacoin on Dire (4+6), got {totals['dire']}"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

