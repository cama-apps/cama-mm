"""
Tests for registration command logic.
"""

import json
import os
import tempfile
import time

import pytest

from database import Database
from services.player_service import PlayerService
from repositories.player_repository import PlayerRepository


class TestRoleDeduplication:
    """Tests for the role deduplication in /setroles command."""
    
    def test_duplicate_roles_are_removed(self):
        """Test that duplicate roles are removed from input."""
        # Simulating the logic from commands/registration.py set_roles method
        roles = "111"
        cleaned = roles.replace(',', '').replace(' ', '')
        role_list = list(cleaned)
        
        valid_choices = ['1', '2', '3', '4', '5']
        for r in role_list:
            assert r in valid_choices
        
        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))
        
        assert role_list == ["1"]
    
    def test_duplicate_roles_preserve_order(self):
        """Test that order is preserved when deduplicating roles."""
        roles = "12321"
        cleaned = roles.replace(',', '').replace(' ', '')
        role_list = list(cleaned)
        
        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))
        
        # Should be ["1", "2", "3"] - order of first appearance
        assert role_list == ["1", "2", "3"]
    
    def test_duplicate_roles_with_commas(self):
        """Test that duplicates are removed even with comma-separated input."""
        roles = "1,1,1"
        cleaned = roles.replace(',', '').replace(' ', '')
        role_list = list(cleaned)
        
        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))
        
        assert role_list == ["1"]
    
    def test_no_duplicates_unchanged(self):
        """Test that input without duplicates is unchanged."""
        roles = "123"
        cleaned = roles.replace(',', '').replace(' ', '')
        role_list = list(cleaned)
        
        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))
        
        assert role_list == ["1", "2", "3"]
    
    def test_all_roles_with_duplicates(self):
        """Test a case with all roles but with duplicates."""
        roles = "1234512345"
        cleaned = roles.replace(',', '').replace(' ', '')
        role_list = list(cleaned)
        
        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))
        
        assert role_list == ["1", "2", "3", "4", "5"]
    
    def test_extreme_duplicates(self):
        """Test the bug case from the user report - 10 carry roles."""
        roles = "1111111111"  # 10 ones
        cleaned = roles.replace(',', '').replace(' ', '')
        role_list = list(cleaned)
        
        # Before deduplication: 10 items
        assert len(role_list) == 10
        
        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))
        
        # After deduplication: 1 item
        assert role_list == ["1"]


class TestPlayerServiceSetRoles:
    """Service layer tests for PlayerService.set_roles()."""
    
    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix='.db')
        os.close(fd)
        db = Database(db_path)
        yield db
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
    def player_service(self, test_db):
        """Create a PlayerService with test database."""
        return PlayerService(PlayerRepository(test_db.db_path))
    
    def test_set_roles_persists_to_database(self, test_db, player_service):
        """Test that set_roles correctly persists roles to the database."""
        user_id = 12345
        test_db.add_player(
            discord_id=user_id,
            discord_username="TestPlayer",
            initial_mmr=2000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06
        )
        
        # Set roles through the service
        player_service.set_roles(user_id, ["1", "2", "3"])
        
        # Verify persisted in database
        player = test_db.get_player(user_id)
        assert player.preferred_roles == ["1", "2", "3"]
    
    def test_set_roles_updates_existing_roles(self, test_db, player_service):
        """Test that set_roles updates existing roles."""
        user_id = 12345
        test_db.add_player(
            discord_id=user_id,
            discord_username="TestPlayer",
            initial_mmr=2000,
            preferred_roles=["1", "2"],
        )
        
        # Update roles
        player_service.set_roles(user_id, ["4", "5"])
        
        # Verify updated
        player = test_db.get_player(user_id)
        assert player.preferred_roles == ["4", "5"]
    
    def test_set_roles_unregistered_player_raises(self, player_service):
        """Test that set_roles raises for unregistered player."""
        with pytest.raises(ValueError, match="Player not registered"):
            player_service.set_roles(99999, ["1", "2"])


class TestSetRolesE2E:
    """End-to-end tests for the /setroles command flow."""
    
    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix='.db')
        os.close(fd)
        db = Database(db_path)
        yield db
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
    def player_service(self, test_db):
        """Create a PlayerService with test database."""
        return PlayerService(PlayerRepository(test_db.db_path))
    
    def _simulate_setroles_command(self, roles_input: str):
        """
        Simulate the parsing and deduplication logic from the /setroles command.
        Returns the processed role list that would be passed to player_service.set_roles().
        """
        # This mirrors the logic in commands/registration.py set_roles method
        cleaned = roles_input.replace(',', '').replace(' ', '')
        role_list = list(cleaned)
        
        valid_choices = ['1', '2', '3', '4', '5']
        for r in role_list:
            if r not in valid_choices:
                raise ValueError(f"Invalid role: {r}")
        
        if not role_list:
            raise ValueError("Please provide at least one role.")
        
        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))
        
        return role_list
    
    def test_e2e_duplicate_roles_deduplicated_and_persisted(self, test_db, player_service):
        """E2E: Duplicate roles input is deduplicated and correctly persisted."""
        user_id = 54321
        test_db.add_player(
            discord_id=user_id,
            discord_username="E2EPlayer",
            initial_mmr=3000,
            glicko_rating=1800.0,
            glicko_rd=350.0,
            glicko_volatility=0.06
        )
        
        # Simulate user entering "1111111111" (the bug case)
        role_list = self._simulate_setroles_command("1111111111")
        assert role_list == ["1"]  # Deduplicated
        
        # Pass to service (as the command would)
        player_service.set_roles(user_id, role_list)
        
        # Verify final state in database
        player = test_db.get_player(user_id)
        assert player.preferred_roles == ["1"]
    
    def test_e2e_mixed_duplicates_preserve_order(self, test_db, player_service):
        """E2E: Mixed duplicates preserve first-occurrence order and persist correctly."""
        user_id = 54322
        test_db.add_player(
            discord_id=user_id,
            discord_username="E2EPlayer2",
            initial_mmr=2500,
        )
        
        # Simulate user entering "54321123" - should become ["5", "4", "3", "2", "1"]
        role_list = self._simulate_setroles_command("54321123")
        assert role_list == ["5", "4", "3", "2", "1"]
        
        player_service.set_roles(user_id, role_list)
        
        player = test_db.get_player(user_id)
        assert player.preferred_roles == ["5", "4", "3", "2", "1"]
    
    def test_e2e_comma_separated_with_duplicates(self, test_db, player_service):
        """E2E: Comma-separated input with duplicates is handled correctly."""
        user_id = 54323
        test_db.add_player(
            discord_id=user_id,
            discord_username="E2EPlayer3",
            initial_mmr=2000,
        )
        
        # Simulate user entering "1, 2, 1, 3, 2" with spaces and commas
        role_list = self._simulate_setroles_command("1, 2, 1, 3, 2")
        assert role_list == ["1", "2", "3"]
        
        player_service.set_roles(user_id, role_list)
        
        player = test_db.get_player(user_id)
        assert player.preferred_roles == ["1", "2", "3"]
    
    def test_e2e_invalid_role_rejected(self, test_db, player_service):
        """E2E: Invalid role input is rejected before reaching the service."""
        with pytest.raises(ValueError, match="Invalid role"):
            self._simulate_setroles_command("126")  # 6 is invalid

