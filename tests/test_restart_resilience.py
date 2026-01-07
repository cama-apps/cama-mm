"""
Tests for bot restart resilience - verifying state survives shutdown/restart.

These tests validate that:
1. Pending match state (after shuffle) is restored from database
2. Active bets are preserved and correctly settled after restart
3. Lobby state is restored from database
"""

import os
import tempfile
import time

import pytest

from domain.models.lobby import LobbyManager
from infrastructure.schema_manager import SchemaManager
from repositories.bet_repository import BetRepository
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService


def _seed_players(player_repo: PlayerRepository, player_ids: list[int], balance: int = 100):
    """Create test players with roles and balance."""
    roles_cycle = [["1"], ["2"], ["3"], ["4"], ["5"]]
    for i, pid in enumerate(player_ids):
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=3000 + (i * 100),
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
            preferred_roles=roles_cycle[i % 5],
        )
        player_repo.update_balance(pid, balance)


class TestRestartResilience:
    """Tests that verify state survives simulated restarts."""

    @pytest.fixture
    def db_path(self):
        """Create a temporary database with schema."""
        fd, path = tempfile.mkstemp(suffix=".db")
        os.close(fd)
        SchemaManager(path).initialize()
        yield path
        time.sleep(0.1)
        try:
            os.unlink(path)
        except OSError:
            pass

    def test_pending_match_restored_after_restart(self, db_path):
        """Test that pending match state is restored from DB after service restart."""
        guild_id = 1234

        # --- Phase 1: Create shuffle (simulating bot running) ---
        player_repo_1 = PlayerRepository(db_path)
        match_repo_1 = MatchRepository(db_path)
        match_service_1 = MatchService(
            player_repo=player_repo_1,
            match_repo=match_repo_1,
            use_glicko=False,
        )

        player_ids = list(range(1000, 1010))
        _seed_players(player_repo_1, player_ids)

        # Perform shuffle
        result = match_service_1.shuffle_players(player_ids, guild_id=guild_id)
        assert "radiant_team" in result  # Returns Team objects

        # Get the persisted state which has team IDs
        persisted_state = match_service_1.get_last_shuffle(guild_id)
        assert persisted_state is not None
        original_radiant = persisted_state["radiant_team_ids"]
        original_dire = persisted_state["dire_team_ids"]

        # --- Phase 2: Simulate restart (new service instances) ---
        player_repo_2 = PlayerRepository(db_path)
        match_repo_2 = MatchRepository(db_path)
        match_service_2 = MatchService(
            player_repo=player_repo_2,
            match_repo=match_repo_2,
            use_glicko=False,
        )

        # Verify state is restored from database (lazy load)
        restored_state = match_service_2.get_last_shuffle(guild_id)
        assert restored_state is not None, "Pending match should be restored after restart"
        assert restored_state["radiant_team_ids"] == original_radiant
        assert restored_state["dire_team_ids"] == original_dire

    def test_bets_survive_restart_and_settle_correctly(self, db_path):
        """Test that bets placed before restart are correctly settled after restart."""
        guild_id = 5678

        # --- Phase 1: Shuffle and place bets ---
        player_repo_1 = PlayerRepository(db_path)
        match_repo_1 = MatchRepository(db_path)
        bet_repo_1 = BetRepository(db_path)
        betting_service_1 = BettingService(bet_repo_1, player_repo_1)
        match_service_1 = MatchService(
            player_repo=player_repo_1,
            match_repo=match_repo_1,
            use_glicko=False,
            betting_service=betting_service_1,
        )

        player_ids = list(range(2000, 2010))
        _seed_players(player_repo_1, player_ids, balance=100)

        # Create a spectator who will bet
        spectator_id = 9999
        player_repo_1.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
        player_repo_1.update_balance(spectator_id, 50)

        # Shuffle with betting enabled
        match_service_1.shuffle_players(
            player_ids, guild_id=guild_id, betting_mode="house"
        )

        # Get the persisted state (which has team IDs and timestamps)
        pending_state = match_service_1.get_last_shuffle(guild_id)
        assert pending_state is not None

        # Extend betting window to allow bets
        now_ts = int(time.time())
        pending_state["bet_lock_until"] = now_ts + 600
        pending_state["shuffle_timestamp"] = now_ts - 10
        # Use match_service to persist (handles Team object serialization)
        match_service_1._persist_match_state(guild_id, pending_state)

        # Spectator places a bet on Radiant
        bet_repo_1.place_bet_against_pending_match_atomic(
            guild_id=guild_id,
            discord_id=spectator_id,
            team="radiant",
            amount=20,
            bet_time=now_ts,
        )

        # Verify bet was placed
        pre_bets = bet_repo_1.get_player_pending_bets(guild_id, spectator_id)
        assert len(pre_bets) == 1
        assert pre_bets[0]["amount"] == 20
        assert pre_bets[0]["team_bet_on"] == "radiant"

        # Check balance was debited
        spectator_balance_before = player_repo_1.get_by_id(spectator_id).jopacoin_balance
        assert spectator_balance_before == 30  # 50 - 20

        # --- Phase 2: Simulate restart ---
        player_repo_2 = PlayerRepository(db_path)
        match_repo_2 = MatchRepository(db_path)
        bet_repo_2 = BetRepository(db_path)
        betting_service_2 = BettingService(bet_repo_2, player_repo_2)
        match_service_2 = MatchService(
            player_repo=player_repo_2,
            match_repo=match_repo_2,
            use_glicko=False,
            betting_service=betting_service_2,
        )

        # Verify pending match is restored
        restored_state = match_service_2.get_last_shuffle(guild_id)
        assert restored_state is not None

        # Verify bet still exists and is pending
        restored_bets = bet_repo_2.get_player_pending_bets(guild_id, spectator_id)
        assert len(restored_bets) == 1
        assert restored_bets[0]["amount"] == 20

        # --- Phase 3: Record match result after restart ---
        record_result = match_service_2.record_match(
            winning_team="radiant",
            guild_id=guild_id,
        )
        assert record_result["match_id"] is not None  # Successfully recorded

        # Verify bet was settled correctly (spectator won)
        # House mode: win returns stake + profit = 20 + 20 = 40
        spectator_balance_after = player_repo_2.get_by_id(spectator_id).jopacoin_balance
        assert spectator_balance_after == 70  # 30 + 40 (stake + winnings)

        # Verify no more pending bets
        post_bets = bet_repo_2.get_player_pending_bets(guild_id, spectator_id)
        assert len(post_bets) == 0

    def test_lobby_state_restored_after_restart(self, db_path):
        """Test that lobby state is restored from DB after restart."""
        # --- Phase 1: Create lobby and add players ---
        lobby_repo_1 = LobbyRepository(db_path)
        lobby_manager_1 = LobbyManager(lobby_repo_1)

        creator_id = 111
        lobby = lobby_manager_1.get_or_create_lobby(creator_id)
        assert lobby.created_by == creator_id

        # Add players to lobby
        player_ids = [222, 333, 444, 555]
        for pid in player_ids:
            assert lobby_manager_1.join_lobby(pid)

        # Set lobby message info
        lobby_manager_1.set_lobby_message(message_id=99999, channel_id=88888)

        # Verify state
        assert lobby_manager_1.lobby.get_player_count() == 4
        assert lobby_manager_1.lobby_message_id == 99999
        assert lobby_manager_1.lobby_channel_id == 88888

        # --- Phase 2: Simulate restart ---
        lobby_repo_2 = LobbyRepository(db_path)
        lobby_manager_2 = LobbyManager(lobby_repo_2)

        # Verify lobby is restored
        restored_lobby = lobby_manager_2.get_lobby()
        assert restored_lobby is not None, "Lobby should be restored after restart"
        assert restored_lobby.created_by == creator_id
        assert restored_lobby.get_player_count() == 4
        assert set(restored_lobby.players) == set(player_ids)

        # Verify message info is restored
        assert lobby_manager_2.lobby_message_id == 99999
        assert lobby_manager_2.lobby_channel_id == 88888

    def test_full_workflow_survives_restart(self, db_path):
        """
        End-to-end test: shuffle -> bet -> restart -> record -> verify settlement.

        This simulates the exact scenario the user asked about:
        1. Active lobby game exists (shuffle was run)
        2. Active bets exist
        3. Bot restarts
        4. All tracking should be restored from DB
        """
        guild_id = 42

        # --- Phase 1: Set up game with bets ---
        player_repo_1 = PlayerRepository(db_path)
        match_repo_1 = MatchRepository(db_path)
        bet_repo_1 = BetRepository(db_path)
        lobby_repo_1 = LobbyRepository(db_path)
        betting_service_1 = BettingService(bet_repo_1, player_repo_1)
        match_service_1 = MatchService(
            player_repo=player_repo_1,
            match_repo=match_repo_1,
            use_glicko=False,
            betting_service=betting_service_1,
        )
        lobby_manager_1 = LobbyManager(lobby_repo_1)

        # Create players and add to lobby
        player_ids = list(range(3000, 3010))
        _seed_players(player_repo_1, player_ids, balance=50)

        for pid in player_ids:
            lobby_manager_1.join_lobby(pid)

        # Create spectators who will bet
        spectator_radiant = 8001
        spectator_dire = 8002
        for sid in [spectator_radiant, spectator_dire]:
            player_repo_1.add(
                discord_id=sid,
                discord_username=f"Spectator{sid}",
                initial_mmr=3000,
                glicko_rating=1500.0,
                glicko_rd=200.0,
                glicko_volatility=0.06,
            )
            player_repo_1.update_balance(sid, 100)

        # Shuffle
        match_service_1.shuffle_players(
            player_ids, guild_id=guild_id, betting_mode="house"
        )

        # Get the persisted state (which has team IDs and timestamps)
        pending_state = match_service_1.get_last_shuffle(guild_id)
        assert pending_state is not None

        # Extend betting window
        now_ts = int(time.time())
        pending_state["bet_lock_until"] = now_ts + 600
        pending_state["shuffle_timestamp"] = now_ts - 10
        # Use match_service to persist (handles Team object serialization)
        match_service_1._persist_match_state(guild_id, pending_state)

        # Place bets from spectators
        bet_repo_1.place_bet_against_pending_match_atomic(
            guild_id=guild_id,
            discord_id=spectator_radiant,
            team="radiant",
            amount=30,
            bet_time=now_ts,
        )
        bet_repo_1.place_bet_against_pending_match_atomic(
            guild_id=guild_id,
            discord_id=spectator_dire,
            team="dire",
            amount=25,
            bet_time=now_ts,
        )

        # Verify pre-restart state
        assert match_service_1.get_last_shuffle(guild_id) is not None
        assert len(bet_repo_1.get_player_pending_bets(guild_id, spectator_radiant)) == 1
        assert len(bet_repo_1.get_player_pending_bets(guild_id, spectator_dire)) == 1
        assert lobby_manager_1.lobby.get_player_count() == 10

        # Record balances before restart
        radiant_balance_pre = player_repo_1.get_by_id(spectator_radiant).jopacoin_balance
        dire_balance_pre = player_repo_1.get_by_id(spectator_dire).jopacoin_balance
        assert radiant_balance_pre == 70  # 100 - 30
        assert dire_balance_pre == 75  # 100 - 25

        # --- Phase 2: RESTART (new instances, same DB) ---
        player_repo_2 = PlayerRepository(db_path)
        match_repo_2 = MatchRepository(db_path)
        bet_repo_2 = BetRepository(db_path)
        lobby_repo_2 = LobbyRepository(db_path)
        betting_service_2 = BettingService(bet_repo_2, player_repo_2)
        match_service_2 = MatchService(
            player_repo=player_repo_2,
            match_repo=match_repo_2,
            use_glicko=False,
            betting_service=betting_service_2,
        )
        lobby_manager_2 = LobbyManager(lobby_repo_2)

        # Verify all state restored
        restored_match = match_service_2.get_last_shuffle(guild_id)
        assert restored_match is not None, "Pending match not restored!"
        assert restored_match["radiant_team_ids"] == pending_state["radiant_team_ids"]
        assert restored_match["dire_team_ids"] == pending_state["dire_team_ids"]

        restored_bets_r = bet_repo_2.get_player_pending_bets(guild_id, spectator_radiant)
        restored_bets_d = bet_repo_2.get_player_pending_bets(guild_id, spectator_dire)
        assert len(restored_bets_r) == 1, "Radiant spectator bet not restored!"
        assert len(restored_bets_d) == 1, "Dire spectator bet not restored!"

        restored_lobby = lobby_manager_2.get_lobby()
        assert restored_lobby is not None, "Lobby not restored!"
        assert restored_lobby.get_player_count() == 10

        # --- Phase 3: Record match result after restart ---
        record_result = match_service_2.record_match(
            winning_team="dire",
            guild_id=guild_id,
        )
        assert record_result["match_id"] is not None  # Successfully recorded

        # Verify bet settlement
        # Radiant spectator LOST: balance stays at 70 (already debited)
        # Dire spectator WON: gets 25 + 25 = 50 back, so 75 + 50 = 125
        radiant_final = player_repo_2.get_by_id(spectator_radiant).jopacoin_balance
        dire_final = player_repo_2.get_by_id(spectator_dire).jopacoin_balance

        assert radiant_final == 70, f"Radiant loser balance wrong: {radiant_final}"
        assert dire_final == 125, f"Dire winner balance wrong: {dire_final}"

        # Verify all bets are settled (no pending bets)
        assert len(bet_repo_2.get_player_pending_bets(guild_id, spectator_radiant)) == 0
        assert len(bet_repo_2.get_player_pending_bets(guild_id, spectator_dire)) == 0

        # Verify pending match is cleared
        assert match_service_2.get_last_shuffle(guild_id) is None
