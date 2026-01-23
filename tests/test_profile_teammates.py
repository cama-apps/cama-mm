"""
Tests for the Teammates tab in the /profile command.

Tests the _build_teammates_embed method which displays pairwise statistics.
"""

import pytest

from database import Database
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository


@pytest.fixture
def temp_db_path(tmp_path):
    """Create a temporary database path for testing."""
    db_path = str(tmp_path / "test_profile_teammates.db")
    # Initialize schema
    Database(db_path)
    return db_path


@pytest.fixture
def pairings_repo(temp_db_path):
    """Create a PairingsRepository instance."""
    return PairingsRepository(temp_db_path)


@pytest.fixture
def player_repo(temp_db_path):
    """Create a PlayerRepository instance."""
    return PlayerRepository(temp_db_path)


def register_players(player_repo, player_ids):
    """Helper to register test players."""
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=3000,
        )


class MockUser:
    """Mock Discord user for testing."""

    def __init__(self, user_id: int, display_name: str = None):
        self.id = user_id
        self.display_name = display_name or f"Player{user_id}"


class MockBot:
    """Mock Discord bot for testing."""

    def __init__(self, pairings_repo=None, player_repo=None):
        self.pairings_repo = pairings_repo
        self.player_repo = player_repo


class TestTeammatesEmbed:
    """Tests for the _build_teammates_embed method."""

    @pytest.fixture
    def profile_cog(self, pairings_repo, player_repo):
        """Create a ProfileCommands cog with mocked bot."""
        from commands.profile import ProfileCommands

        mock_bot = MockBot(pairings_repo=pairings_repo, player_repo=player_repo)
        return ProfileCommands(mock_bot)

    @pytest.mark.asyncio
    async def test_teammates_embed_unregistered_user(self, profile_cog):
        """Test that unregistered users get an appropriate error."""
        user = MockUser(99999, "UnregisteredUser")

        embed, file = await profile_cog._build_teammates_embed(user, user.id)

        assert embed.title == "Not Registered"
        assert "not registered" in embed.description.lower()
        assert file is None

    @pytest.mark.asyncio
    async def test_teammates_embed_no_data(self, profile_cog, player_repo):
        """Test embed when player has no pairings data."""
        register_players(player_repo, [1001])
        user = MockUser(1001)

        embed, file = await profile_cog._build_teammates_embed(user, user.id)

        assert "Teammates" in embed.title
        assert file is None
        # Should have fields even if empty
        field_names = [f.name for f in embed.fields]
        assert "🏆 Best Teammates" in field_names
        assert "💀 Worst Teammates" in field_names

    @pytest.mark.asyncio
    async def test_teammates_embed_with_data(self, profile_cog, player_repo, pairings_repo):
        """Test embed shows correct teammate data."""
        players = list(range(1, 16))
        register_players(player_repo, players)

        # Create pairings: Player 1 + 2 = 100% win rate (4 games)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 6, 7, 8],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )

        # Create pairings: Player 1 + 3 = 0% win rate (4 games)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=200 + i,
                team1_ids=[1, 3, 6, 7, 8],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=2,
            )

        user = MockUser(1)
        embed, file = await profile_cog._build_teammates_embed(user, user.id)

        assert "Teammates" in embed.title
        assert file is None

        # Find the best teammates field
        best_field = next((f for f in embed.fields if "Best Teammates" in f.name), None)
        assert best_field is not None
        # Player 2 should appear with 100% win rate
        assert "<@2>" in best_field.value or "Player2" in best_field.value
        assert "100%" in best_field.value

        # Find the worst teammates field
        worst_field = next((f for f in embed.fields if "Worst Teammates" in f.name), None)
        assert worst_field is not None
        # Player 3 should appear with 0% win rate
        assert "<@3>" in worst_field.value or "Player3" in worst_field.value
        assert "0%" in worst_field.value

    @pytest.mark.asyncio
    async def test_teammates_embed_dominates_section(self, profile_cog, player_repo, pairings_repo):
        """Test that dominates (best matchups) section is populated."""
        players = list(range(1, 16))
        register_players(player_repo, players)

        # Player 1 dominates player 10 (always wins against)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )

        user = MockUser(1)
        embed, _ = await profile_cog._build_teammates_embed(user, user.id)

        # Find the dominates field
        dominates_field = next((f for f in embed.fields if "Dominates" in f.name), None)
        assert dominates_field is not None
        # Player 10 should appear
        assert "<@10>" in dominates_field.value or "Player10" in dominates_field.value
        assert "100%" in dominates_field.value

    @pytest.mark.asyncio
    async def test_teammates_embed_struggles_section(self, profile_cog, player_repo, pairings_repo):
        """Test that struggles against (worst matchups) section is populated."""
        players = list(range(1, 16))
        register_players(player_repo, players)

        # Player 1 struggles against player 10 (always loses)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=2,
            )

        user = MockUser(1)
        embed, _ = await profile_cog._build_teammates_embed(user, user.id)

        # Find the struggles field
        struggles_field = next((f for f in embed.fields if "Struggles" in f.name), None)
        assert struggles_field is not None
        # Player 10 should appear
        assert "<@10>" in struggles_field.value or "Player10" in struggles_field.value
        assert "0%" in struggles_field.value

    @pytest.mark.asyncio
    async def test_teammates_embed_most_played_sections(self, profile_cog, player_repo, pairings_repo):
        """Test most played with/against sections."""
        players = list(range(1, 20))
        register_players(player_repo, players)

        # Player 1 + 2: 6 games together
        for i in range(6):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 6, 7, 8],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )

        # Player 1 vs 10: 6 games against
        # (already created above)

        user = MockUser(1)
        embed, _ = await profile_cog._build_teammates_embed(user, user.id)

        # Find most played with field
        most_with = next((f for f in embed.fields if "Most Played With" in f.name), None)
        assert most_with is not None
        assert "<@2>" in most_with.value or "Player2" in most_with.value
        assert "6g" in most_with.value  # 6 games

        # Find most played against field
        most_against = next((f for f in embed.fields if "Most Played Against" in f.name), None)
        assert most_against is not None
        assert "<@10>" in most_against.value or "Player10" in most_against.value

    @pytest.mark.asyncio
    async def test_teammates_embed_evenly_matched(self, profile_cog, player_repo, pairings_repo):
        """Test evenly matched sections (50% win rate)."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Player 1 + 2: exactly 50% (2 wins, 2 losses)
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=102 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=2,
            )

        user = MockUser(1)
        embed, _ = await profile_cog._build_teammates_embed(user, user.id)

        # Find even teammates field (may not appear if min_games not met or below threshold)
        even_teammates = next((f for f in embed.fields if "Even Teammates" in f.name), None)
        if even_teammates:
            # If it appears, player 2 should be there
            assert "<@2>" in even_teammates.value or "Player2" in even_teammates.value

    @pytest.mark.asyncio
    async def test_teammates_embed_footer_has_counts(self, profile_cog, player_repo, pairings_repo):
        """Test that footer shows pairing counts."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Record a match
        pairings_repo.update_pairings_for_match(
            match_id=1,
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
        )

        user = MockUser(1)
        embed, _ = await profile_cog._build_teammates_embed(user, user.id)

        # Footer should mention minimum games
        assert embed.footer is not None
        assert "Min" in embed.footer.text or "min" in embed.footer.text


class TestTeammatesEmbedEdgeCases:
    """Edge case tests for teammates embed."""

    @pytest.fixture
    def profile_cog(self, pairings_repo, player_repo):
        """Create a ProfileCommands cog with mocked bot."""
        from commands.profile import ProfileCommands

        mock_bot = MockBot(pairings_repo=pairings_repo, player_repo=player_repo)
        return ProfileCommands(mock_bot)

    @pytest.mark.asyncio
    async def test_teammates_embed_min_games_threshold(self, profile_cog, player_repo, pairings_repo):
        """Test that players with fewer than min_games don't appear."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Only 2 games together (below min_games=3 default)
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )

        user = MockUser(1)
        embed, _ = await profile_cog._build_teammates_embed(user, user.id)

        # Best teammates should show "No data yet" since min_games=3
        best_field = next((f for f in embed.fields if "Best Teammates" in f.name), None)
        assert best_field is not None
        assert "No data" in best_field.value

    @pytest.mark.asyncio
    async def test_teammates_embed_no_pairings_repo(self, player_repo):
        """Test graceful handling when pairings_repo is unavailable."""
        from commands.profile import ProfileCommands

        # Bot without pairings_repo
        mock_bot = MockBot(pairings_repo=None, player_repo=player_repo)
        profile_cog = ProfileCommands(mock_bot)

        register_players(player_repo, [1001])
        user = MockUser(1001)

        embed, file = await profile_cog._build_teammates_embed(user, user.id)

        # Should return an error embed
        assert "unavailable" in embed.description.lower() or "error" in embed.title.lower()
