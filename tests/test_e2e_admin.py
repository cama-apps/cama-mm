"""
End-to-end admin and per-guild state tests.

Sections:
  - Admin commands (from original test_e2e_admin.py)
  - Exclusion tracking (from test_e2e_exclusion.py)
  - Per-guild match state (from test_e2e_guild_state.py)
  - Soft avoid (from test_soft_avoid_e2e.py)
  - RD decay integration (from test_rd_decay_integration.py)
"""

import math
import random
from datetime import UTC, datetime, timedelta
from unittest.mock import AsyncMock, Mock, patch

import pytest
from discord import app_commands

from commands.admin import AdminCommands
from commands.match import MatchCommands
from config import NEW_PLAYER_EXCLUSION_BOOST
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.soft_avoid_repository import SoftAvoidRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from services.match_service import MatchService
from services.player_service import PlayerService
from shuffler import BalancedShuffler
from tests.conftest import TEST_GUILD_ID
from tests.repository_harness import RepositoryTestDatabase as Database

# =============================================================================
# Helpers (hoisted from source files)
# =============================================================================


class MockDiscordUser:
    """Mock Discord user for testing."""

    def __init__(self, user_id, username="TestUser"):
        self.id = user_id
        self.name = username
        self.display_name = username
        self.mention = f"<@{user_id}>"

    def __str__(self):
        return self.name


class MockDiscordInteraction:
    """Mock Discord interaction for testing."""

    def __init__(self, user_id, username="TestUser"):
        self.user = MockDiscordUser(user_id, username)
        self.response = AsyncMock()
        self.followup = AsyncMock()
        self.channel = Mock()
        self.guild = None

    async def defer(self, **kwargs):
        """Mock defer response."""
        pass


def _expected_after_exclusions(exclusions: int) -> int:
    """Helper: expected exclusion count given N exclusions from a fresh player."""
    return NEW_PLAYER_EXCLUSION_BOOST + exclusions * 6


def _create_guild_state_players(
    player_repo: PlayerRepository, guild_id: int, start_id: int = 60000, count: int = 10
):
    """Helper used by the per-guild match state section."""
    player_ids = list(range(start_id, start_id + count))
    for idx, pid in enumerate(player_ids):
        player_repo.add(
            discord_id=pid,
            discord_username=f"TestGuildPlayer{pid}",
            guild_id=guild_id,
            initial_mmr=1600 + idx * 10,
            glicko_rating=1600.0 + idx * 2,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
    return player_ids


def _register_soft_avoid_players(
    player_repo: PlayerRepository, guild_id: int = 0, count: int = 10
) -> list[int]:
    """Register test players for soft avoid scenarios and return their discord IDs."""
    discord_ids = []
    for i in range(count):
        discord_id = 1000 + i
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"Player{i + 1}",
            guild_id=guild_id,
            initial_mmr=3000 + i * 100,
            glicko_rating=1500.0,
            glicko_rd=100.0,
            glicko_volatility=0.06,
        )
        # Set preferred roles
        player_repo.update_roles(discord_id, guild_id, ["1", "2", "3", "4", "5"])
        discord_ids.append(discord_id)
    return discord_ids


# =============================================================================
# Section: Admin commands (from original test_e2e_admin.py)
# =============================================================================


class TestE2EAdminCommands:
    """Tests for admin command permission checks."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.mark.asyncio
    async def test_resetuser_requires_admin(self, test_db):
        """/admin resetuser rejects non-admins and only deletes for admins.

        Exercises the real has_admin_permission gate inside the resetuser
        callback by constructing interactions with real (non-)admin guild
        permissions — the permission check itself is not patched.
        """
        from types import SimpleNamespace

        player_repo = PlayerRepository(test_db.db_path)
        player_service = PlayerService(player_repo)
        # Unique guild id keeps the resetuser rate-limit bucket isolated.
        guild_id = 424242

        target_id = 200101
        player_repo.add(
            discord_id=target_id,
            discord_username="UserToReset",
            guild_id=guild_id,
            initial_mmr=1500,
        )

        admin_cmd = AdminCommands(
            bot=None, lobby_service=None, player_service=player_service
        )
        target_member = SimpleNamespace(
            id=target_id, mention=f"<@{target_id}>", send=AsyncMock()
        )
        mock_guild = SimpleNamespace(id=guild_id)

        # Non-admin: command must refuse and leave the player untouched.
        non_admin = MockDiscordInteraction(880001, "RegularUser")
        non_admin.guild = mock_guild
        non_admin.user.guild_permissions = SimpleNamespace(
            administrator=False, manage_guild=False
        )

        await admin_cmd.resetuser.callback(admin_cmd, non_admin, target_member)

        call = non_admin.followup.send.call_args
        message = call.kwargs.get("content") or (call.args[0] if call.args else "")
        assert "Admin only" in message
        assert player_repo.get_by_id(target_id, guild_id) is not None

        # Positive control: the same command with admin permissions deletes.
        admin = MockDiscordInteraction(880002, "AdminUser")
        admin.guild = mock_guild
        admin.user.guild_permissions = SimpleNamespace(
            administrator=True, manage_guild=False
        )

        await admin_cmd.resetuser.callback(admin_cmd, admin, target_member)

        call = admin.followup.send.call_args
        message = call.kwargs.get("content") or (call.args[0] if call.args else "")
        assert "Reset" in message
        assert player_repo.get_by_id(target_id, guild_id) is None

    @pytest.mark.asyncio
    @pytest.mark.timeout(60)
    async def test_admin_override_record_command(self, test_db):
        """Test end-to-end admin override via /record command."""
        # Create services first so we can use player_repo
        lobby_repo = LobbyRepository(test_db.db_path)
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)

        # Create 10 players using PlayerRepository with guild_id
        player_ids = list(range(600001, 600011))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        match_service = MatchService(
            player_repo=player_repo, match_repo=match_repo, use_glicko=True
        )
        lobby_manager = LobbyManager(lobby_repo)
        lobby_service = LobbyService(lobby_manager, player_repo)
        player_service = PlayerService(player_repo)

        # Create a mock bot
        mock_bot = Mock()
        mock_bot.db = test_db
        mock_bot.lobby_service = lobby_service
        mock_bot.match_service = match_service
        mock_bot.player_service = player_service

        # Shuffle players to create a pending match
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        # Verify match is pending
        assert match_service.get_last_shuffle(TEST_GUILD_ID) is not None
        assert match_service.can_record_match(TEST_GUILD_ID) is False  # No submissions yet

        # Create admin interaction
        admin_id = 999999
        mock_interaction = MockDiscordInteraction(admin_id, "AdminUser")

        # Mock guild
        from types import SimpleNamespace

        mock_guild = SimpleNamespace(id=TEST_GUILD_ID)
        mock_interaction.guild = mock_guild

        # Mock admin permissions
        mock_permissions = SimpleNamespace(administrator=True, manage_guild=False)
        mock_interaction.user.guild_permissions = mock_permissions

        # Mock the result choice
        result_choice = app_commands.Choice(name="Radiant Won", value="radiant")

        # Create MatchCommands instance
        match_commands = MatchCommands(mock_bot, lobby_service, match_service, player_service)

        # Patch has_admin_permission to return True for our admin
        with patch("commands.match.has_admin_permission", return_value=True):
            # Call the record method directly (bypassing the command decorator)
            # The decorator wraps it, but we can call the underlying method
            await match_commands.record.callback(match_commands, mock_interaction, result_choice)

        # Verify the followup was called
        assert mock_interaction.followup.send.called

        # Get the message that was sent
        call_args = mock_interaction.followup.send.call_args
        message = call_args[0][0] if call_args[0] else call_args[1].get("content", "")

        assert "Match recorded" in message

        # Verify match was actually recorded (state cleared)
        assert match_service.get_last_shuffle(TEST_GUILD_ID) is None

        # Verify that the match was recorded in the database
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT COUNT(*) as count FROM matches")
        match_count = cursor.fetchone()["count"]
        conn.close()

        assert match_count > 0, "Match should have been recorded in database"

        # Verify that players have updated ratings (Glicko ratings should have changed)
        # At least some players should have updated ratings
        updated_ratings = 0
        for pid in player_ids:
            rating_data = test_db.get_player_glicko_rating(pid, guild_id=TEST_GUILD_ID)
            if rating_data:
                rating, rd, _ = rating_data
                # Initial rating was 1500.0, after a match it should have changed
                if rating != 1500.0 or rd != 350.0:
                    updated_ratings += 1

        assert updated_ratings > 0, "At least some players should have updated ratings after match"

    @pytest.mark.asyncio
    @pytest.mark.timeout(60)
    async def test_non_admin_record_requires_3_submissions(self, test_db):
        """Test that non-admin /record command requires 3 submissions."""
        test_guild_id = 12346  # Different guild ID for this test

        # Create services first so we can use player_repo
        lobby_repo = LobbyRepository(test_db.db_path)
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)

        # Create 10 players using PlayerRepository with guild_id
        player_ids = list(range(600101, 600111))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=test_guild_id,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        match_service = MatchService(
            player_repo=player_repo, match_repo=match_repo, use_glicko=True
        )
        lobby_manager = LobbyManager(lobby_repo)
        lobby_service = LobbyService(lobby_manager, player_repo)
        player_service = PlayerService(player_repo)

        # Create a mock bot
        mock_bot = Mock()
        mock_bot.db = test_db
        mock_bot.lobby_service = lobby_service
        mock_bot.match_service = match_service
        mock_bot.player_service = player_service

        # Shuffle players to create a pending match
        match_service.shuffle_players(player_ids, guild_id=test_guild_id)

        # Create non-admin interaction
        user_id = 600101  # a shuffled participant - non-participants cannot vote
        mock_interaction = MockDiscordInteraction(user_id, "RegularUser")

        # Mock guild
        from types import SimpleNamespace

        mock_guild = SimpleNamespace(id=test_guild_id)
        mock_interaction.guild = mock_guild

        # Mock non-admin permissions
        mock_permissions = SimpleNamespace(administrator=False, manage_guild=False)
        mock_interaction.user.guild_permissions = mock_permissions

        # Mock the result choice
        result_choice = app_commands.Choice(name="Radiant Won", value="radiant")

        # Create MatchCommands instance
        match_commands = MatchCommands(mock_bot, lobby_service, match_service, player_service)

        # Patch has_admin_permission to return False for non-admin
        with patch("commands.match.has_admin_permission", return_value=False):
            # First submission - should not be ready
            await match_commands.record.callback(match_commands, mock_interaction, result_choice)

            # Verify message indicates vote was recorded and shows counts
            call_args = mock_interaction.followup.send.call_args
            message = call_args[0][0] if call_args[0] else call_args[1].get("content", "")
            assert "Result recorded" in message
            assert "1/3" in message or "Radiant: 1/3" in message

            # Verify match is still pending
            assert match_service.get_last_shuffle(test_guild_id) is not None
            assert match_service.can_record_match(test_guild_id) is False

            # Second submission from different user
            mock_interaction2 = MockDiscordInteraction(600102, "User2")
            mock_interaction2.guild = mock_guild
            mock_interaction2.user.guild_permissions = mock_permissions
            mock_interaction2.response = AsyncMock()
            mock_interaction2.followup = AsyncMock()

            await match_commands.record.callback(match_commands, mock_interaction2, result_choice)

            # Still not ready
            assert match_service.can_record_match(test_guild_id) is False

            # Third submission from different user
            mock_interaction3 = MockDiscordInteraction(600103, "User3")
            mock_interaction3.guild = mock_guild
            mock_interaction3.user.guild_permissions = mock_permissions
            mock_interaction3.response = AsyncMock()
            mock_interaction3.followup = AsyncMock()

            # Before third submission, should not be ready
            assert match_service.can_record_match(test_guild_id) is False
            assert match_service.get_non_admin_submission_count(test_guild_id) == 2

            await match_commands.record.callback(match_commands, mock_interaction3, result_choice)

            # After third submission, match should be recorded (state cleared)
            # Check the message to see if it was recorded
            call_args = mock_interaction3.followup.send.call_args
            message = call_args[0][0] if call_args[0] else call_args[1].get("content", "")

            # Should have recorded the match (not just a submission message)
            assert "Match recorded" in message, (
                f"Expected 'Match recorded' in message, got: {message}"
            )
            # State should be cleared after recording
            assert match_service.get_last_shuffle(test_guild_id) is None


# =============================================================================
# Section: Exclusion tracking (from test_e2e_exclusion.py)
# =============================================================================


class TestE2EExclusionTracking:
    """End-to-end tests for exclusion count tracking feature."""

    def test_exclusion_count_workflow(self, test_db_memory):
        """
        Test the complete exclusion tracking workflow:
        1. Create 12 players
        2. Shuffle (2 excluded, 10 included)
        3. Verify excluded players' counts increment
        4. Verify included players' counts decay
        5. Shuffle again and verify new selection
        """
        # Step 1: Create 12 players
        player_ids = list(range(400001, 400013))
        player_names = [f"Player{i}" for i in range(1, 13)]

        for pid, name in zip(player_ids, player_names):
            test_db_memory.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Step 2: Get initial exclusion counts (all should equal the boost value)
        initial_counts = test_db_memory.get_exclusion_counts(player_ids)
        for pid in player_ids:
            assert (
                initial_counts[pid] == NEW_PLAYER_EXCLUSION_BOOST
            ), f"Player {pid} should start with {NEW_PLAYER_EXCLUSION_BOOST} exclusion count"

        # Step 3: Shuffle from pool (simulate first shuffle)
        players = test_db_memory.get_players_by_ids(player_ids)
        shuffler = BalancedShuffler(
            use_glicko=True, off_role_flat_penalty=50.0, exclusion_penalty_weight=5.0
        )

        # Get exclusion counts for shuffler
        exclusion_counts_by_id = test_db_memory.get_exclusion_counts(player_ids)
        exclusion_counts = {
            pl.name: exclusion_counts_by_id[pid] for pid, pl in zip(player_ids, players)
        }

        team1, team2, excluded_players = shuffler.shuffle_from_pool(players, exclusion_counts)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded_players) == 2

        # Step 4: Track which players were included/excluded
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        included_player_ids = [player_name_to_id[p.name] for p in team1.players + team2.players]
        excluded_player_ids = [player_name_to_id[p.name] for p in excluded_players]

        assert len(included_player_ids) == 10
        assert len(excluded_player_ids) == 2
        assert set(included_player_ids).isdisjoint(set(excluded_player_ids))

        # Step 5: Simulate bot behavior - increment excluded, decay included
        for pid in excluded_player_ids:
            test_db_memory.increment_exclusion_count(pid)
        for pid in included_player_ids:
            test_db_memory.decay_exclusion_count(pid)

        # Step 6: Verify counts changed correctly
        updated_counts = test_db_memory.get_exclusion_counts(player_ids)

        expected_after_first = _expected_after_exclusions(1)
        for pid in excluded_player_ids:
            assert (
                updated_counts[pid] == expected_after_first
            ), f"Excluded player {pid} should have count {expected_after_first}"

        for pid in included_player_ids:
            assert (
                updated_counts[pid] < NEW_PLAYER_EXCLUSION_BOOST
            ), f"Included player {pid} should have decayed below {NEW_PLAYER_EXCLUSION_BOOST}"

        # Step 7: Increment the same excluded players again — accumulation
        # across multiple exclusions must be exact.
        # (Covers the multi-cycle accumulation previously exercised by
        # test_multiple_shuffle_cycles_with_exclusions.)
        for pid in excluded_player_ids:
            test_db_memory.increment_exclusion_count(pid)
            test_db_memory.increment_exclusion_count(pid)
            test_db_memory.increment_exclusion_count(pid)

        # Now excluded players have count=_expected_after_exclusions(4), others have lower counts
        second_counts = test_db_memory.get_exclusion_counts(player_ids)
        expected_after_second = _expected_after_exclusions(4)
        for pid in excluded_player_ids:
            assert (
                second_counts[pid] == expected_after_second
            ), f"Excluded player {pid} should have count {expected_after_second}"

    def test_exclusion_decay_subtracts_one_per_inclusion(self, test_db_memory):
        """
        Test that included players lose one exclusion-factor point per game.
        """
        # Create 11 players (1 will always be excluded in this test)
        player_ids = list(range(400201, 400212))
        for pid in player_ids:
            test_db_memory.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Artificially set one player to have high exclusion count
        unlucky_player_id = player_ids[0]
        for _ in range(16):  # Start with count of boost + 96 (16 * 6 per exclusion)
            test_db_memory.increment_exclusion_count(unlucky_player_id)

        initial_counts = test_db_memory.get_exclusion_counts(player_ids)
        value = _expected_after_exclusions(16)
        assert initial_counts[unlucky_player_id] == value

        for _ in range(7):
            test_db_memory.decay_exclusion_count(unlucky_player_id)
            after_decay = test_db_memory.get_exclusion_counts([unlucky_player_id])
            value -= 1
            assert after_decay[unlucky_player_id] == value


# =============================================================================
# Section: Per-guild match state (from test_e2e_guild_state.py)
# =============================================================================


@pytest.fixture
def match_test_db(repo_db_path):
    """Create a Database for match service tests using centralized fast fixture."""
    return Database(repo_db_path)


class TestE2EGuildIdMatchState:
    """Ensure shuffle state is tracked per guild."""

    def test_shuffle_and_record_is_scoped_per_guild(self, match_test_db):
        """Shuffle/record state is per-guild, including the guild_id=0 (DM)
        bucket, and recording from the wrong guild raises.

        Merged from test_shuffle_and_record_with_guild_id,
        test_shuffle_and_record_without_guild_id, and
        test_shuffle_and_record_different_guilds.
        """
        player_repo = PlayerRepository(match_test_db.db_path)
        match_repo = MatchRepository(match_test_db.db_path)
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo)

        # Guild 42: shuffle, then a record from a different guild must not
        # find (or clear) the pending shuffle.
        guild_id = 42
        player_ids = _create_guild_state_players(player_repo, guild_id=guild_id, start_id=50000)
        match_service.shuffle_players(player_ids, guild_id=guild_id)
        assert match_service.get_last_shuffle(guild_id) is not None

        with pytest.raises(ValueError, match="No recent shuffle found."):
            match_service.record_match("radiant", guild_id=2)
        assert match_service.get_last_shuffle(guild_id) is not None

        result = match_service.record_match("radiant", guild_id=guild_id)
        assert result["winning_team"] == "radiant"
        assert match_service.get_last_shuffle(guild_id) is None

        # guild_id=0 (DM / default bucket) works the same way.
        dm_player_ids = _create_guild_state_players(player_repo, guild_id=0, start_id=60000)
        match_service.shuffle_players(dm_player_ids, guild_id=0)
        assert match_service.get_last_shuffle(0) is not None

        result = match_service.record_match("dire", guild_id=0)
        assert result["winning_team"] == "dire"
        assert match_service.get_last_shuffle(0) is None


# =============================================================================
# Section: Soft avoid (from test_soft_avoid_e2e.py)
# =============================================================================


@pytest.fixture
def soft_avoid_repo(repo_db_path):
    """Create a SoftAvoidRepository with temp database."""
    return SoftAvoidRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    """Create a PlayerRepository with temp database."""
    return PlayerRepository(repo_db_path)


@pytest.fixture
def match_repo(repo_db_path):
    """Create a MatchRepository with temp database."""
    return MatchRepository(repo_db_path)


@pytest.fixture
def match_service(player_repo, match_repo, soft_avoid_repo):
    """Create a MatchService with soft avoid support."""
    return MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        soft_avoid_repo=soft_avoid_repo,
    )


class TestE2ESoftAvoid:
    """End-to-end tests for soft avoid feature."""

    def test_avoid_not_decremented_after_shuffle_only(self, match_service, soft_avoid_repo, player_repo):
        """Avoids are NOT decremented after shuffle (only after record_match),
        and bidirectional avoids coexist untouched.

        Merged from test_bidirectional_avoids_work (reciprocal avoids created
        here) and test_shuffle_with_avoids_loads_and_uses (shuffle result shape
        asserted below). Repeated shuffles run WITHOUT clearing the pending
        shuffle in between — re-rolling over a live pending shuffle is the
        production path, and it must still not decrement.
        """
        guild_id = 123
        discord_ids = _register_soft_avoid_players(player_repo, guild_id=guild_id, count=10)

        # Create bidirectional avoids
        soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[0],
            avoided_id=discord_ids[1],
            games=10,
        )
        soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[1],
            avoided_id=discord_ids[0],
            games=10,
        )

        # Shuffle repeatedly without recording and without clearing the
        # pending shuffle (each re-roll overwrites the previous one).
        for _ in range(3):
            result = match_service.shuffle_players(
                player_ids=discord_ids,
                guild_id=guild_id,
            )
            assert result is not None
            assert "radiant_team" in result
            assert "dire_team" in result

        # Both avoids should still have 10 games (not decremented yet)
        avoids = soft_avoid_repo.get_active_avoids_for_players(
            guild_id=guild_id,
            player_ids=discord_ids,
        )
        assert len(avoids) == 2
        for avoid in avoids:
            assert avoid.games_remaining == 10

    def test_avoid_decrements_after_record_match_opposite_teams(self, match_service, soft_avoid_repo, player_repo):
        """Avoids decrement after record_match when players are on opposite
        teams, and the effective avoid id was stored in the shuffle state for
        the deferred decrement.

        Merged from test_effective_avoid_ids_stored_in_shuffle_state — same
        seeded opposite-teams scenario; the state assertion happens just
        before the record.
        """
        guild_id = 123
        discord_ids = _register_soft_avoid_players(player_repo, guild_id=guild_id, count=10)

        # Create an avoid
        avoid = soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[0],
            avoided_id=discord_ids[1],
            games=10,
        )

        # Seed randomness to get deterministic shuffle with opposite teams
        random.seed(42)
        max_attempts = 50
        found = False
        for _ in range(max_attempts):
            result = match_service.shuffle_players(
                player_ids=discord_ids,
                guild_id=guild_id,
            )

            radiant_ids = {result["radiant_team"].players[i].discord_id for i in range(5)}

            # Check if they're on opposite teams
            avoider_on_radiant = discord_ids[0] in radiant_ids
            avoided_on_radiant = discord_ids[1] in radiant_ids

            if avoider_on_radiant != avoided_on_radiant:
                # The effective avoid id must be stored in the shuffle state
                # (record_match relies on it for the deferred decrement).
                state = match_service.get_last_shuffle(guild_id)
                assert state is not None
                assert avoid.id in state.effective_avoid_ids

                # They're on opposite teams - record the match
                match_service.record_match(winning_team="radiant", guild_id=guild_id)

                # Now the avoid should be decremented
                avoids = soft_avoid_repo.get_active_avoids_for_players(
                    guild_id=guild_id,
                    player_ids=discord_ids,
                )
                if len(avoids) == 1:
                    assert avoids[0].games_remaining == 9
                    found = True
                    break

            # Clear shuffle state for next attempt (don't record)
            match_service.clear_last_shuffle(guild_id)

        assert found, "Could not get opposite teams after max attempts (seed=42)"

    def test_goodness_score_includes_avoid_penalty(self, match_service, soft_avoid_repo, player_repo):
        """Test that goodness_score includes soft avoid penalty when pair is on same team."""
        guild_id = 123
        discord_ids = _register_soft_avoid_players(player_repo, guild_id=guild_id, count=10)

        # Create avoids (multiple to increase chance of penalty)
        avoid_pairs = [
            (discord_ids[0], discord_ids[1]),
            (discord_ids[2], discord_ids[3]),
            (discord_ids[4], discord_ids[5]),
        ]
        for avoider_id, avoided_id in avoid_pairs:
            soft_avoid_repo.create_or_reactivate_avoid(
                guild_id=guild_id,
                avoider_id=avoider_id,
                avoided_id=avoided_id,
                games=10,
            )

        # Shuffle
        result = match_service.shuffle_players(
            player_ids=discord_ids,
            guild_id=guild_id,
        )

        radiant_ids = {p.discord_id for p in result["radiant_team"].players}
        dire_ids = {p.discord_id for p in result["dire_team"].players}
        same_team_avoids = sum(
            (avoider in radiant_ids and avoided in radiant_ids)
            or (avoider in dire_ids and avoided in dire_ids)
            for avoider, avoided in avoid_pairs
        )
        shuffler = match_service.shuffler
        selected_players = result["radiant_team"].players + result["dire_team"].players
        selected_values = [
            p.get_value(
                shuffler.use_glicko,
                use_openskill=shuffler.use_openskill,
                use_jopacoin=shuffler.use_jopacoin,
            )
            for p in selected_players
        ]
        expected_score = (
            same_team_avoids * shuffler.soft_avoid_penalty
            - shuffler._calculate_lobby_rating_bonus(selected_values)
            - shuffler._calculate_rd_priority(selected_players)
        )

        assert result["goodness_score"] == pytest.approx(expected_score)


# =============================================================================
# Section: RD decay integration (from test_rd_decay_integration.py)
# =============================================================================




def test_record_match_updates_ratings_and_last_match_date_without_decay(
    test_db_with_schema,
):
    """Recording a match bulk-applies rating updates AND stamps last_match_date,
    and loading a just-played player does not apply RD decay.

    Merged from test_last_match_date_updated_after_record,
    test_rd_decay_not_applied_when_match_just_recorded, and
    test_bulk_update_and_last_match_date_are_both_applied — one shuffle+record
    pass verifies all three slices.
    """
    player_repo = PlayerRepository(test_db_with_schema.db_path)
    match_repo = MatchRepository(test_db_with_schema.db_path)
    match_service = MatchService(player_repo, match_repo, use_glicko=True)

    # Create 10 players with known RD (below the cap so decay would be visible)
    start_rd = 150.0
    for i in range(10):
        pid = 200 + i
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=start_rd,
            glicko_volatility=0.06,
        )

    player_ids = list(range(200, 210))

    # Shuffle and record match
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
    assert result["winning_team"] == "radiant"

    # All participants: RD decreased (bulk rating update applied) and
    # last_match_date is set and recent.
    for pid in player_ids:
        new_rating = player_repo.get_glicko_rating(pid, TEST_GUILD_ID)
        assert new_rating[1] < start_rd, f"Player {pid} RD should decrease after match"

        dates = player_repo.get_last_match_date(pid, TEST_GUILD_ID)
        assert dates is not None, f"Player {pid} should have dates tuple"
        last_match, _ = dates
        assert last_match is not None, f"Player {pid} should have last_match_date set"
        last_match_dt = datetime.fromisoformat(last_match)
        if last_match_dt.tzinfo is None:
            last_match_dt = last_match_dt.replace(tzinfo=UTC)
        now = datetime.now(UTC)
        assert (now - last_match_dt).total_seconds() < 60, "last_match_date should be recent"

    # Loading a just-played player must not apply decay on top.
    player, _ = match_service._load_glicko_player(200, TEST_GUILD_ID)
    assert player.rd <= start_rd, f"RD should not increase after a match (got {player.rd})"


def test_rd_decay_applied_beyond_grace_period_only(test_db_with_schema):
    """RD decay applies to a player inactive past the grace period, but not to
    one exactly at the seven-day grace boundary.

    Merged from test_rd_decay_applied_for_inactive_player and
    test_rd_decay_respects_grace_period.
    """
    player_repo = PlayerRepository(test_db_with_schema.db_path)
    match_repo = MatchRepository(test_db_with_schema.db_path)
    match_service = MatchService(player_repo, match_repo, use_glicko=True)

    start_rd = 100.0
    for pid, username in ((300, "InactivePlayer"), (400, "RecentPlayer")):
        player_repo.add(
            discord_id=pid,
            discord_username=username,
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=start_rd,
            glicko_volatility=0.06,
        )

    # Inactive: last_match_date 4 weeks ago (beyond grace period) → decayed.
    four_weeks_ago = (datetime.now(UTC) - timedelta(weeks=4)).isoformat()
    player_repo.update_last_match_date(300, TEST_GUILD_ID, four_weeks_ago)

    player, _ = match_service._load_glicko_player(300, TEST_GUILD_ID)
    expected_periods = (4 * 7 - 7) / 7
    expected_rd = math.sqrt(start_rd * start_rd + (100.0 * 100.0) * expected_periods)
    assert math.isclose(player.rd, expected_rd, rel_tol=0.01), \
        f"RD should decay from {start_rd} to ~{expected_rd}, got {player.rd}"

    # Recent: last_match_date at the seven-day grace boundary → untouched.
    one_week_ago = (datetime.now(UTC) - timedelta(weeks=1)).isoformat()
    player_repo.update_last_match_date(400, TEST_GUILD_ID, one_week_ago)

    player, _ = match_service._load_glicko_player(400, TEST_GUILD_ID)
    assert player.rd == start_rd, f"RD should not decay within grace period (got {player.rd})"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
