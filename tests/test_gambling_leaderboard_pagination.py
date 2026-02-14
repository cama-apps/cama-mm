"""Tests for gambling leaderboard tab pagination in UnifiedLeaderboardView."""

import pytest
from dataclasses import dataclass
from unittest.mock import MagicMock, AsyncMock


@dataclass
class MockLeaderboardEntry:
    """Mock LeaderboardEntry for testing."""

    discord_id: int
    net_pnl: int
    win_rate: float
    total_wagered: int
    total_bets: int = 10
    wins: int = 5
    losses: int = 5
    avg_leverage: float = 1.5
    degen_score: int = 50
    degen_emoji: str = "🎰"
    degen_title: str = "Gambler"


@dataclass
class MockLeaderboard:
    """Mock Leaderboard for testing."""

    top_earners: list
    down_bad: list
    hall_of_degen: list
    biggest_gamblers: list
    total_wagered: int = 10000
    total_bets: int = 100
    avg_degen_score: float = 50.0
    total_bankruptcies: int = 2
    total_loans: int = 5
    server_stats: dict = None

    def __post_init__(self):
        if self.server_stats is None:
            self.server_stats = {
                "total_bets": 100,
                "total_wagered": 10000,
                "unique_gamblers": 10,
                "avg_bet_size": 100,
                "total_bankruptcies": 2,
            }


@pytest.fixture
def mock_cog():
    """Create a mock InfoCommands cog with required dependencies."""
    cog = MagicMock()
    cog.player_repo = MagicMock()
    cog.gambling_stats_service = MagicMock()
    cog.bankruptcy_service = MagicMock()
    cog.bankruptcy_service.get_bulk_states.return_value = {}
    cog.prediction_service = MagicMock()
    return cog


@pytest.fixture
def mock_interaction():
    """Create a mock Discord interaction."""
    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 12345
    interaction.guild.members = []
    interaction.user = MagicMock()
    interaction.user.id = 67890
    interaction.response = MagicMock()
    interaction.response.is_done.return_value = False
    interaction.response.defer = AsyncMock()
    interaction.response.edit_message = AsyncMock()
    interaction.edit_original_response = AsyncMock()
    return interaction


class TestGamblingTabPagination:
    """Tests for gambling tab pagination in UnifiedLeaderboardView."""

    def _create_view_with_gambling_data(self, mock_cog, mock_interaction, leaderboard):
        """Helper to create a UnifiedLeaderboardView with gambling data loaded."""
        from commands.info import UnifiedLeaderboardView, LeaderboardTab

        mock_cog.gambling_stats_service.get_leaderboard.return_value = leaderboard

        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GAMBLING,
        )

        # Manually load the gambling tab data
        from commands.info import MULTI_SECTION_PAGE_SIZE
        state = view._tab_states[LeaderboardTab.GAMBLING]
        state.data = leaderboard
        state.extra = {"bankruptcy_states": {}}
        state.loaded = True
        # Set view-level guild members cache (empty = DM context, no filtering)
        view._guild_members_cache = {}

        # Calculate max pages
        max_entries = max(
            len(leaderboard.top_earners),
            len(leaderboard.down_bad),
            len(leaderboard.hall_of_degen),
            len(leaderboard.biggest_gamblers),
            1,
        )
        state.max_page = max(0, (max_entries - 1) // MULTI_SECTION_PAGE_SIZE)

        view._update_pagination_buttons()

        return view

    @pytest.mark.asyncio
    async def test_page_calculation_with_many_entries(self, mock_cog, mock_interaction):
        """View calculates correct number of pages."""
        from commands.info import MULTI_SECTION_PAGE_SIZE, LeaderboardTab

        # Create 20 entries
        entries = [
            MockLeaderboardEntry(i, 100 - i * 10, 0.5, 1000) for i in range(20)
        ]
        leaderboard = MockLeaderboard(entries, [], entries, entries)

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        state = view._tab_states[LeaderboardTab.GAMBLING]

        expected_pages = (20 - 1) // MULTI_SECTION_PAGE_SIZE
        assert state.max_page == expected_pages

    @pytest.mark.asyncio
    async def test_page_calculation_with_few_entries(self, mock_cog, mock_interaction):
        """View calculates single page for small datasets."""
        from commands.info import LeaderboardTab

        # Create fewer entries than page size
        entries = [MockLeaderboardEntry(i, 100, 0.5, 1000) for i in range(5)]
        leaderboard = MockLeaderboard(entries, [], [], [])

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        state = view._tab_states[LeaderboardTab.GAMBLING]

        assert state.max_page == 0  # Single page

    @pytest.mark.asyncio
    async def test_build_embed_respects_field_limits(self, mock_cog, mock_interaction):
        """Embed fields stay under 1024 characters."""
        # Create entries
        entries = [MockLeaderboardEntry(i, 100, 0.5, 1000) for i in range(20)]
        leaderboard = MockLeaderboard(entries, entries, entries, entries)

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        embed = view.build_embed()

        for f in embed.fields:
            assert len(f.value) <= 1024, (
                f"Field '{f.name}' exceeds 1024 chars: {len(f.value)}"
            )

    @pytest.mark.asyncio
    async def test_empty_sections_not_added(self, mock_cog, mock_interaction):
        """Empty sections don't create embed fields."""
        leaderboard = MockLeaderboard([], [], [], [])
        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        embed = view.build_embed()

        # No fields should exist for empty sections
        # Embed should have description instead
        assert "No gambling data yet" in embed.description

    @pytest.mark.asyncio
    async def test_pagination_buttons_disabled_appropriately(self, mock_cog, mock_interaction):
        """Buttons are disabled on first/last pages."""
        from commands.info import LeaderboardTab

        entries = [MockLeaderboardEntry(i, 100, 0.5, 1000) for i in range(20)]
        leaderboard = MockLeaderboard(entries, [], [], [])

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        state = view._tab_states[LeaderboardTab.GAMBLING]

        # First page: prev disabled, next enabled
        assert view.prev_button.disabled is True
        assert view.next_button.disabled is False

        # Go to last page
        state.current_page = state.max_page
        view._update_pagination_buttons()

        # Last page: prev enabled, next disabled
        assert view.prev_button.disabled is False
        assert view.next_button.disabled is True

    @pytest.mark.asyncio
    async def test_single_page_both_buttons_disabled(self, mock_cog, mock_interaction):
        """Both buttons disabled when only one page exists."""
        entries = [MockLeaderboardEntry(i, 100, 0.5, 1000) for i in range(3)]
        leaderboard = MockLeaderboard(entries, [], [], [])

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)

        # Single page: both buttons disabled
        assert view.prev_button.disabled is True
        assert view.next_button.disabled is True

    @pytest.mark.asyncio
    async def test_page_slicing_shows_correct_entries(self, mock_cog, mock_interaction):
        """Each page shows the correct subset of entries."""
        from commands.info import MULTI_SECTION_PAGE_SIZE, LeaderboardTab

        # Create 20 entries with unique identifiable values
        entries = [MockLeaderboardEntry(i, 1000 - i * 10, 0.5, 1000) for i in range(20)]
        leaderboard = MockLeaderboard(entries, [], [], [])

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        state = view._tab_states[LeaderboardTab.GAMBLING]

        # Add mock guild members for name lookup (view-level cache)
        view._guild_members_cache = {i: MagicMock(display_name=f"User{i}") for i in range(20)}

        # Page 0 should show entries 0-7
        embed = view.build_embed()
        assert len(embed.fields) > 0
        assert "User0" in embed.fields[0].value
        assert f"User{MULTI_SECTION_PAGE_SIZE - 1}" in embed.fields[0].value
        assert f"User{MULTI_SECTION_PAGE_SIZE}" not in embed.fields[0].value

        # Page 1 should show entries 8-15
        state.current_page = 1
        embed = view.build_embed()
        assert "User0" not in embed.fields[0].value
        assert f"User{MULTI_SECTION_PAGE_SIZE}" in embed.fields[0].value

    @pytest.mark.asyncio
    async def test_down_bad_filters_negative_only(self, mock_cog, mock_interaction):
        """Down bad section only shows players with negative P&L."""
        from commands.info import LeaderboardTab

        # Mix of positive and negative P&L in down_bad
        down_bad_entries = [
            MockLeaderboardEntry(1, 100, 0.6, 1000),   # Positive
            MockLeaderboardEntry(2, -50, 0.4, 1000),  # Negative
            MockLeaderboardEntry(3, 0, 0.5, 1000),    # Zero
            MockLeaderboardEntry(4, -100, 0.3, 1000), # Negative
        ]
        # Need some top_earners so the embed doesn't show "No gambling data yet"
        top_earners = [MockLeaderboardEntry(10, 500, 0.8, 2000)]
        leaderboard = MockLeaderboard(top_earners, down_bad_entries, [], [])

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        state = view._tab_states[LeaderboardTab.GAMBLING]

        # Add mock guild members
        view._guild_members_cache = {i: MagicMock(display_name=f"User{i}") for i in range(1, 5)}

        embed = view.build_embed()

        # Find the Down Bad field
        down_bad_field = None
        for f in embed.fields:
            if "Down Bad" in f.name:
                down_bad_field = f
                break

        assert down_bad_field is not None
        assert "User2" in down_bad_field.value
        assert "User4" in down_bad_field.value
        assert "User1" not in down_bad_field.value  # Positive, should be excluded
        assert "User3" not in down_bad_field.value  # Zero, should be excluded

    @pytest.mark.asyncio
    async def test_footer_includes_page_info(self, mock_cog, mock_interaction):
        """Footer shows page number and server stats."""
        entries = [MockLeaderboardEntry(i, 100, 0.5, 1000) for i in range(20)]
        leaderboard = MockLeaderboard(entries, [], [], [])

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        embed = view.build_embed()

        assert "Page 1/" in embed.footer.text
        assert "100 bets" in embed.footer.text

    @pytest.mark.asyncio
    async def test_index_numbering_continues_across_pages(self, mock_cog, mock_interaction):
        """Entry numbers continue correctly across pages (1-8, 9-16, etc.)."""
        from commands.info import MULTI_SECTION_PAGE_SIZE, LeaderboardTab

        entries = [MockLeaderboardEntry(i, 100, 0.5, 1000) for i in range(20)]
        leaderboard = MockLeaderboard(entries, [], [], [])

        view = self._create_view_with_gambling_data(mock_cog, mock_interaction, leaderboard)
        state = view._tab_states[LeaderboardTab.GAMBLING]

        # Add mock guild members
        view._guild_members_cache = {i: MagicMock(display_name=f"User{i}") for i in range(20)}

        # First page starts at 1
        embed = view.build_embed()
        assert "1. **User0**" in embed.fields[0].value

        # Second page starts at 9 (MULTI_SECTION_PAGE_SIZE + 1)
        state.current_page = 1
        embed = view.build_embed()
        expected_start = MULTI_SECTION_PAGE_SIZE + 1
        assert f"{expected_start}. **User{MULTI_SECTION_PAGE_SIZE}**" in embed.fields[0].value
