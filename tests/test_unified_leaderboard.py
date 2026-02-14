"""
Tests for UnifiedLeaderboardView in commands/info.py.

Tests cover:
- Tab switching updates button styles
- Per-tab page state is independent
- Lazy loading only fetches once per tab
- Predictions pagination works
- Deep-linking via type parameter works
"""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch
from dataclasses import dataclass

# Import the classes we're testing
from commands.info import (
    LeaderboardTab,
    TabState,
    UnifiedLeaderboardView,
    SINGLE_SECTION_PAGE_SIZE,
    MULTI_SECTION_PAGE_SIZE,
)


@pytest.fixture
def mock_cog():
    """Create a mock InfoCommands cog with required dependencies."""
    cog = MagicMock()

    # Mock player_repo
    cog.player_repo = MagicMock()
    cog.player_repo.get_leaderboard.return_value = []
    cog.player_repo.get_player_count.return_value = 0
    cog.player_repo.get_players_with_negative_balance.return_value = []
    cog.player_repo.get_leaderboard_by_glicko.return_value = []
    cog.player_repo.get_leaderboard_by_openskill.return_value = []
    cog.player_repo.get_rated_player_count.return_value = 0

    # Mock gambling_stats_service
    cog.gambling_stats_service = MagicMock()
    mock_leaderboard = MagicMock()
    mock_leaderboard.top_earners = []
    mock_leaderboard.down_bad = []
    mock_leaderboard.hall_of_degen = []
    mock_leaderboard.biggest_gamblers = []
    mock_leaderboard.server_stats = {}
    cog.gambling_stats_service.get_leaderboard.return_value = mock_leaderboard

    # Mock prediction_service
    cog.prediction_service = MagicMock()
    cog.prediction_service.get_prediction_leaderboard.return_value = {
        "top_earners": [],
        "down_bad": [],
        "most_accurate": [],
    }
    cog.prediction_service.get_server_prediction_stats.return_value = {
        "total_predictions": 0,
        "total_bets": 0,
        "total_wagered": 0,
    }

    # Mock bankruptcy_service
    cog.bankruptcy_service = MagicMock()
    cog.bankruptcy_service.get_bulk_states.return_value = {}

    return cog


def create_mock_member(discord_id: int, name: str = None) -> MagicMock:
    """Create a mock guild member."""
    member = MagicMock()
    member.id = discord_id
    member.display_name = name or f"User{discord_id}"
    return member


@pytest.fixture
def mock_interaction():
    """Create a mock Discord interaction."""
    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 12345
    # Include mock members for common test IDs (123, 456, 789, 67890)
    interaction.guild.members = [
        create_mock_member(123, "User123"),
        create_mock_member(456, "User456"),
        create_mock_member(789, "User789"),
        create_mock_member(67890, "TestUser"),
    ]
    interaction.user = MagicMock()
    interaction.user.id = 67890
    interaction.response = MagicMock()
    interaction.response.is_done.return_value = False
    interaction.response.defer = AsyncMock()
    interaction.response.edit_message = AsyncMock()
    interaction.edit_original_response = AsyncMock()
    return interaction


class TestLeaderboardTab:
    """Tests for LeaderboardTab enum."""

    def test_tab_values(self):
        """Test all tab enum values are correct."""
        assert LeaderboardTab.BALANCE.value == "balance"
        assert LeaderboardTab.GAMBLING.value == "gambling"
        assert LeaderboardTab.PREDICTIONS.value == "predictions"
        assert LeaderboardTab.GLICKO.value == "glicko"
        assert LeaderboardTab.OPENSKILL.value == "openskill"
        assert LeaderboardTab.TIPS.value == "tips"

    def test_tab_count(self):
        """Test we have exactly 6 tabs."""
        assert len(LeaderboardTab) == 6


class TestTabState:
    """Tests for TabState dataclass."""

    def test_default_values(self):
        """Test TabState default values."""
        state = TabState()
        assert state.data is None
        assert state.current_page == 0
        assert state.max_page == 0
        assert state.loaded is False
        assert state.extra == {}

    def test_custom_values(self):
        """Test TabState with custom values."""
        state = TabState(
            data={"test": "data"},
            current_page=2,
            max_page=5,
            loaded=True,
            extra={"key": "value"},
        )
        assert state.data == {"test": "data"}
        assert state.current_page == 2
        assert state.max_page == 5
        assert state.loaded is True
        assert state.extra == {"key": "value"}


class TestUnifiedLeaderboardViewInitialization:
    """Tests for UnifiedLeaderboardView initialization."""

    @pytest.mark.asyncio
    async def test_initial_tab_balance(self, mock_cog, mock_interaction):
        """Test view initializes with Balance tab by default."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )
        assert view.current_tab == LeaderboardTab.BALANCE

    @pytest.mark.asyncio
    async def test_initial_tab_custom(self, mock_cog, mock_interaction):
        """Test view initializes with custom tab."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GAMBLING,
        )
        assert view.current_tab == LeaderboardTab.GAMBLING

    @pytest.mark.asyncio
    async def test_all_tabs_have_state(self, mock_cog, mock_interaction):
        """Test all tabs have their own state."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )
        assert len(view._tab_states) == 6
        for tab in LeaderboardTab:
            assert tab in view._tab_states
            assert isinstance(view._tab_states[tab], TabState)


class TestButtonStyles:
    """Tests for button style updates."""

    @pytest.mark.asyncio
    async def test_active_tab_is_primary(self, mock_cog, mock_interaction):
        """Test active tab button has primary style."""
        import discord

        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.BALANCE,
        )

        # Balance should be primary, others secondary
        assert view.balance_btn.style == discord.ButtonStyle.primary
        assert view.gambling_btn.style == discord.ButtonStyle.secondary
        assert view.predictions_btn.style == discord.ButtonStyle.secondary
        assert view.glicko_btn.style == discord.ButtonStyle.secondary
        assert view.openskill_btn.style == discord.ButtonStyle.secondary

    @pytest.mark.asyncio
    async def test_tab_switch_updates_styles(self, mock_cog, mock_interaction):
        """Test switching tabs updates button styles."""
        import discord

        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.BALANCE,
        )

        # Switch to gambling tab
        view.current_tab = LeaderboardTab.GAMBLING
        view._update_button_styles()

        assert view.balance_btn.style == discord.ButtonStyle.secondary
        assert view.gambling_btn.style == discord.ButtonStyle.primary
        assert view.predictions_btn.style == discord.ButtonStyle.secondary


class TestPerTabPagination:
    """Tests for per-tab independent pagination."""

    @pytest.mark.asyncio
    async def test_tabs_have_independent_pages(self, mock_cog, mock_interaction):
        """Test each tab maintains its own page state."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )

        # Set different pages for different tabs
        view._tab_states[LeaderboardTab.BALANCE].current_page = 2
        view._tab_states[LeaderboardTab.GAMBLING].current_page = 0
        view._tab_states[LeaderboardTab.GLICKO].current_page = 5

        # Verify they are independent
        assert view._tab_states[LeaderboardTab.BALANCE].current_page == 2
        assert view._tab_states[LeaderboardTab.GAMBLING].current_page == 0
        assert view._tab_states[LeaderboardTab.GLICKO].current_page == 5

    @pytest.mark.asyncio
    async def test_pagination_buttons_reflect_current_tab(self, mock_cog, mock_interaction):
        """Test pagination buttons update based on current tab's page state."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )

        # Balance tab: page 0 of 3
        view._tab_states[LeaderboardTab.BALANCE].current_page = 0
        view._tab_states[LeaderboardTab.BALANCE].max_page = 3
        view._tab_states[LeaderboardTab.BALANCE].loaded = True

        view.current_tab = LeaderboardTab.BALANCE
        view._update_pagination_buttons()

        assert view.prev_button.disabled is True  # Can't go back from page 0
        assert view.next_button.disabled is False  # Can go forward

        # Now set to last page
        view._tab_states[LeaderboardTab.BALANCE].current_page = 3
        view._update_pagination_buttons()

        assert view.prev_button.disabled is False  # Can go back
        assert view.next_button.disabled is True  # Can't go forward from last page


class TestLazyLoading:
    """Tests for lazy loading behavior."""

    @pytest.mark.asyncio
    async def test_data_not_loaded_initially(self, mock_cog, mock_interaction):
        """Test tabs start with loaded=False."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )

        for tab in LeaderboardTab:
            assert view._tab_states[tab].loaded is False

    @pytest.mark.asyncio
    async def test_load_marks_tab_as_loaded(self, mock_cog, mock_interaction):
        """Test loading data marks tab as loaded."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )

        await view._load_tab_data(LeaderboardTab.BALANCE)

        assert view._tab_states[LeaderboardTab.BALANCE].loaded is True
        # Other tabs should still be not loaded
        assert view._tab_states[LeaderboardTab.GAMBLING].loaded is False

    @pytest.mark.asyncio
    async def test_second_load_does_not_refetch(self, mock_cog, mock_interaction):
        """Test loading an already-loaded tab doesn't refetch."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )

        # First load
        await view._load_tab_data(LeaderboardTab.BALANCE)
        call_count = mock_cog.player_repo.get_leaderboard.call_count

        # Second load (should be no-op)
        await view._load_tab_data(LeaderboardTab.BALANCE)

        # Should not have called again
        assert mock_cog.player_repo.get_leaderboard.call_count == call_count


class TestEmbedBuilding:
    """Tests for embed building."""

    @pytest.mark.asyncio
    async def test_build_balance_embed_empty(self, mock_cog, mock_interaction):
        """Test Balance embed with no data."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
        )

        await view._load_tab_data(LeaderboardTab.BALANCE)
        embed = view.build_embed()

        assert "Balance" in embed.title
        assert embed.description == "No players registered yet!"

    @pytest.mark.asyncio
    async def test_build_gambling_embed_empty(self, mock_cog, mock_interaction):
        """Test Gambling embed with no data."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GAMBLING,
        )

        await view._load_tab_data(LeaderboardTab.GAMBLING)
        embed = view.build_embed()

        assert "Gambling" in embed.title
        assert "No gambling data yet" in embed.description

    @pytest.mark.asyncio
    async def test_build_predictions_embed_empty(self, mock_cog, mock_interaction):
        """Test Predictions embed with no data."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.PREDICTIONS,
        )

        await view._load_tab_data(LeaderboardTab.PREDICTIONS)
        embed = view.build_embed()

        assert "Predictions" in embed.title
        assert "No prediction data yet" in embed.description

    @pytest.mark.asyncio
    async def test_build_glicko_embed_empty(self, mock_cog, mock_interaction):
        """Test Glicko embed with no data."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GLICKO,
        )

        await view._load_tab_data(LeaderboardTab.GLICKO)
        embed = view.build_embed()

        assert "Glicko" in embed.title
        assert "No players with Glicko-2 ratings yet" in embed.description

    @pytest.mark.asyncio
    async def test_build_openskill_embed_empty(self, mock_cog, mock_interaction):
        """Test OpenSkill embed with no data."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.OPENSKILL,
        )

        await view._load_tab_data(LeaderboardTab.OPENSKILL)
        embed = view.build_embed()

        assert "OpenSkill" in embed.title
        assert "No players with OpenSkill ratings yet" in embed.description


class TestPageSizeConstants:
    """Tests for page size constants."""

    def test_single_section_page_size(self):
        """Test single section page size is 20."""
        assert SINGLE_SECTION_PAGE_SIZE == 20

    def test_multi_section_page_size(self):
        """Test multi section page size is 8."""
        assert MULTI_SECTION_PAGE_SIZE == 8


class TestDeepLinking:
    """Tests for deep-linking via type parameter."""

    @pytest.mark.asyncio
    async def test_balance_deep_link(self, mock_cog, mock_interaction):
        """Test initializing with balance tab."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.BALANCE,
        )
        assert view.current_tab == LeaderboardTab.BALANCE

    @pytest.mark.asyncio
    async def test_gambling_deep_link(self, mock_cog, mock_interaction):
        """Test initializing with gambling tab."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GAMBLING,
        )
        assert view.current_tab == LeaderboardTab.GAMBLING

    @pytest.mark.asyncio
    async def test_predictions_deep_link(self, mock_cog, mock_interaction):
        """Test initializing with predictions tab."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.PREDICTIONS,
        )
        assert view.current_tab == LeaderboardTab.PREDICTIONS

    @pytest.mark.asyncio
    async def test_glicko_deep_link(self, mock_cog, mock_interaction):
        """Test initializing with glicko tab."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GLICKO,
        )
        assert view.current_tab == LeaderboardTab.GLICKO

    @pytest.mark.asyncio
    async def test_openskill_deep_link(self, mock_cog, mock_interaction):
        """Test initializing with openskill tab."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.OPENSKILL,
        )
        assert view.current_tab == LeaderboardTab.OPENSKILL

    @pytest.mark.asyncio
    async def test_tips_deep_link(self, mock_cog, mock_interaction):
        """Test initializing with tips tab."""
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.TIPS,
        )
        assert view.current_tab == LeaderboardTab.TIPS


class TestTipsTab:
    """Tests for the Tips leaderboard tab."""

    @pytest.fixture
    def mock_cog_with_tips(self, mock_cog):
        """Create a mock cog with tip_service."""
        mock_tip_service = MagicMock()
        mock_tip_service.get_top_senders.return_value = []
        mock_tip_service.get_top_receivers.return_value = []
        mock_tip_service.get_total_tip_volume.return_value = {
            "total_amount": 0,
            "total_fees": 0,
            "total_transactions": 0,
        }
        mock_cog.bot = MagicMock()
        mock_cog.bot.tip_service = mock_tip_service
        return mock_cog

    @pytest.mark.asyncio
    async def test_build_tips_embed_empty(self, mock_cog_with_tips, mock_interaction):
        """Test Tips embed with no data."""
        view = UnifiedLeaderboardView(
            cog=mock_cog_with_tips,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.TIPS,
        )

        await view._load_tab_data(LeaderboardTab.TIPS)
        embed = view.build_embed()

        assert "Tips" in embed.title
        assert "No tips yet" in embed.description

    @pytest.mark.asyncio
    async def test_build_tips_embed_with_data(self, mock_cog_with_tips, mock_interaction):
        """Test Tips embed with data."""
        # Set up mock data
        mock_cog_with_tips.bot.tip_service.get_top_senders.return_value = [
            {"discord_id": 123, "total_amount": 100, "tip_count": 5},
            {"discord_id": 456, "total_amount": 50, "tip_count": 3},
        ]
        mock_cog_with_tips.bot.tip_service.get_top_receivers.return_value = [
            {"discord_id": 789, "total_amount": 80, "tip_count": 4},
        ]
        mock_cog_with_tips.bot.tip_service.get_total_tip_volume.return_value = {
            "total_amount": 150,
            "total_fees": 15,
            "total_transactions": 8,
        }

        view = UnifiedLeaderboardView(
            cog=mock_cog_with_tips,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.TIPS,
        )

        await view._load_tab_data(LeaderboardTab.TIPS)
        embed = view.build_embed()

        assert "Tips" in embed.title
        # Check for expected fields
        field_names = [field.name for field in embed.fields]
        assert "💝 Most Generous" in field_names
        assert "⭐ Fan Favorites" in field_names
        assert "📊 Server Stats" in field_names

    @pytest.mark.asyncio
    async def test_tips_tab_no_service(self, mock_cog, mock_interaction):
        """Test Tips tab when tip_service is not available."""
        mock_cog.bot = MagicMock()
        mock_cog.bot.tip_service = None

        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.TIPS,
        )

        await view._load_tab_data(LeaderboardTab.TIPS)
        embed = view.build_embed()

        assert "Tips" in embed.title
        assert "not available" in embed.description
