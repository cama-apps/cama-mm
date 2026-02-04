"""
Tests for registration command logic.
"""

import os
import tempfile
import time

import pytest

from database import Database
from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService

TEST_GUILD_ID = 12345


class TestRoleDeduplication:
    """Tests for the role deduplication in /setroles command."""

    def test_duplicate_roles_are_removed(self):
        """Test that duplicate roles are removed from input."""
        # Simulating the logic from commands/registration.py set_roles method
        roles = "111"
        cleaned = roles.replace(",", "").replace(" ", "")
        role_list = list(cleaned)

        valid_choices = ["1", "2", "3", "4", "5"]
        for r in role_list:
            assert r in valid_choices

        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))

        assert role_list == ["1"]

    def test_duplicate_roles_preserve_order(self):
        """Test that order is preserved when deduplicating roles."""
        roles = "12321"
        cleaned = roles.replace(",", "").replace(" ", "")
        role_list = list(cleaned)

        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))

        # Should be ["1", "2", "3"] - order of first appearance
        assert role_list == ["1", "2", "3"]

    def test_duplicate_roles_with_commas(self):
        """Test that duplicates are removed even with comma-separated input."""
        roles = "1,1,1"
        cleaned = roles.replace(",", "").replace(" ", "")
        role_list = list(cleaned)

        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))

        assert role_list == ["1"]

    def test_no_duplicates_unchanged(self):
        """Test that input without duplicates is unchanged."""
        roles = "123"
        cleaned = roles.replace(",", "").replace(" ", "")
        role_list = list(cleaned)

        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))

        assert role_list == ["1", "2", "3"]

    def test_all_roles_with_duplicates(self):
        """Test a case with all roles but with duplicates."""
        roles = "1234512345"
        cleaned = roles.replace(",", "").replace(" ", "")
        role_list = list(cleaned)

        # Deduplicate while preserving order
        role_list = list(dict.fromkeys(role_list))

        assert role_list == ["1", "2", "3", "4", "5"]

    def test_extreme_duplicates(self):
        """Test the bug case from the user report - 10 carry roles."""
        roles = "1111111111"  # 10 ones
        cleaned = roles.replace(",", "").replace(" ", "")
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
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def player_service(self, test_db):
        """Create a PlayerService with test database."""
        return PlayerService(PlayerRepository(test_db.db_path))

    def test_set_roles_persists_to_database(self, test_db, player_service):
        """Test that set_roles correctly persists roles to the database."""
        user_id = 12346  # Different ID to avoid collision with TEST_GUILD_ID
        player_repo = PlayerRepository(test_db.db_path)
        player_repo.add(
            discord_id=user_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Set roles through the service
        player_service.set_roles(user_id, TEST_GUILD_ID, ["1", "2", "3"])

        # Verify persisted in database
        player = player_repo.get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["1", "2", "3"]

    def test_set_roles_updates_existing_roles(self, test_db, player_service):
        """Test that set_roles updates existing roles."""
        user_id = 12347
        player_repo = PlayerRepository(test_db.db_path)
        player_repo.add(
            discord_id=user_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2000,
            preferred_roles=["1", "2"],
        )

        # Update roles
        player_service.set_roles(user_id, TEST_GUILD_ID, ["4", "5"])

        # Verify updated
        player = player_repo.get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["4", "5"]

    def test_set_roles_unregistered_player_raises(self, player_service):
        """Test that set_roles raises for unregistered player."""
        with pytest.raises(ValueError, match="Player not registered"):
            player_service.set_roles(99999, TEST_GUILD_ID, ["1", "2"])


class TestMMRPromptViewSignature:
    """Tests for the MMRPromptView button callback signature.

    The button callback must have (self, interaction, button) parameter order.
    Discord.py passes interaction first, then button. A previous bug had
    these reversed, causing "This interaction failed" errors.
    """

    def test_enter_mmr_button_callback_has_correct_signature(self):
        """Test that discord.ui.button callbacks have (interaction, button) order.

        The correct signature is:
            async def callback(self, interaction: discord.Interaction, button: discord.ui.Button)

        If the parameters are reversed (button, interaction), discord.py will pass
        the wrong types and cause 'This interaction failed' errors when users
        click buttons.

        This test verifies that all button callbacks in the codebase follow the
        correct pattern by checking against a known working example.
        """
        import ast
        import re
        from pathlib import Path

        # Read the registration.py file and find the enter_mmr callback
        registration_path = Path(__file__).parent.parent / "commands" / "registration.py"
        # Be explicit about encoding for Windows compatibility (Path.read_text() defaults
        # to the locale encoding, which may choke on UTF-8 byte sequences).
        source = registration_path.read_text(encoding="utf-8")

        # Find the enter_mmr function definition
        # Pattern: async def enter_mmr(self, <param1>: <type1>, <param2>: <type2>)
        pattern = r"async\s+def\s+enter_mmr\s*\(\s*self\s*,\s*(\w+)\s*:\s*([\w\.]+)\s*,\s*(\w+)\s*:\s*([\w\.]+)"
        match = re.search(pattern, source)

        assert match is not None, "Could not find enter_mmr callback in registration.py"

        param1_name = match.group(1)
        param1_type = match.group(2)
        param2_name = match.group(3)
        param2_type = match.group(4)

        # First parameter should be the interaction
        assert "Interaction" in param1_type or param1_name.lower().startswith("interaction"), (
            f"First parameter should be Interaction type, "
            f"but found '{param1_name}: {param1_type}'. "
            f"Parameters may be in wrong order (should be interaction, then button)."
        )

        # Second parameter should be the button
        assert "Button" in param2_type or param2_name.lower() == "button", (
            f"Second parameter should be Button type, "
            f"but found '{param2_name}: {param2_type}'. "
            f"Parameters may be in wrong order (should be interaction, then button)."
        )


class TestSetRolesE2E:
    """End-to-end tests for the /setroles command flow."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

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
        cleaned = roles_input.replace(",", "").replace(" ", "")
        role_list = list(cleaned)

        valid_choices = ["1", "2", "3", "4", "5"]
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
        player_repo = PlayerRepository(test_db.db_path)
        player_repo.add(
            discord_id=user_id,
            discord_username="E2EPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1800.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Simulate user entering "1111111111" (the bug case)
        role_list = self._simulate_setroles_command("1111111111")
        assert role_list == ["1"]  # Deduplicated

        # Pass to service (as the command would)
        player_service.set_roles(user_id, TEST_GUILD_ID, role_list)

        # Verify final state in database
        player = player_repo.get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["1"]

    def test_e2e_mixed_duplicates_preserve_order(self, test_db, player_service):
        """E2E: Mixed duplicates preserve first-occurrence order and persist correctly."""
        user_id = 54322
        player_repo = PlayerRepository(test_db.db_path)
        player_repo.add(
            discord_id=user_id,
            discord_username="E2EPlayer2",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2500,
        )

        # Simulate user entering "54321123" - should become ["5", "4", "3", "2", "1"]
        role_list = self._simulate_setroles_command("54321123")
        assert role_list == ["5", "4", "3", "2", "1"]

        player_service.set_roles(user_id, TEST_GUILD_ID, role_list)

        player = player_repo.get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["5", "4", "3", "2", "1"]

    def test_e2e_comma_separated_with_duplicates(self, test_db, player_service):
        """E2E: Comma-separated input with duplicates is handled correctly."""
        user_id = 54323
        player_repo = PlayerRepository(test_db.db_path)
        player_repo.add(
            discord_id=user_id,
            discord_username="E2EPlayer3",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2000,
        )

        # Simulate user entering "1, 2, 1, 3, 2" with spaces and commas
        role_list = self._simulate_setroles_command("1, 2, 1, 3, 2")
        assert role_list == ["1", "2", "3"]

        player_service.set_roles(user_id, TEST_GUILD_ID, role_list)

        player = player_repo.get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["1", "2", "3"]

    def test_e2e_invalid_role_rejected(self, test_db, player_service):
        """E2E: Invalid role input is rejected before reaching the service."""
        with pytest.raises(ValueError, match="Invalid role"):
            self._simulate_setroles_command("126")  # 6 is invalid
