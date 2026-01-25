"""
Regression tests for bug fixes:
- Issue 3: Abort recording slowness (15-second sleep removed)
- Issue 2C: Gambling leaderboard timeout (pre-fetch guild members)
- Issue 1: Profile embed spacer for proper layout
"""

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.pairings_repository import PairingsRepository
from services.bankruptcy_service import BankruptcyRepository, BankruptcyService
from services.gambling_stats_service import GamblingStatsService, Leaderboard, LeaderboardEntry


@pytest.fixture
def db_path(tmp_path):
    """Create a temporary database with schema."""
    db = str(tmp_path / "test_bugfix.db")
    schema = SchemaManager(db)
    schema.initialize()
    return db


@pytest.fixture
def repositories(db_path):
    """Create repositories for testing."""
    return {
        "player_repo": PlayerRepository(db_path),
        "bet_repo": BetRepository(db_path),
        "match_repo": MatchRepository(db_path),
        "bankruptcy_repo": BankruptcyRepository(db_path),
        "pairings_repo": PairingsRepository(db_path),
    }


class TestAbortLobbySleepRemoved:
    """
    Regression test for Issue 3: Abort recording slowness.

    The 15-second asyncio.sleep was removed from _abort_lobby_thread() to make
    abort operations complete immediately instead of blocking for 15 seconds.
    """

    @pytest.mark.asyncio
    async def test_abort_lobby_thread_no_long_sleep(self):
        """Verify _abort_lobby_thread doesn't call asyncio.sleep with 15 seconds."""
        from commands.match import MatchCommands

        # Create mock bot and services
        mock_bot = MagicMock()
        mock_lobby_service = MagicMock()
        mock_match_service = MagicMock()
        mock_player_service = MagicMock()

        # Setup mock thread
        mock_thread = AsyncMock()
        mock_thread.send = AsyncMock()
        mock_thread.edit = AsyncMock()
        mock_bot.get_channel = MagicMock(return_value=mock_thread)

        # Setup pending state with thread ID
        mock_match_service.get_last_shuffle.return_value = {
            "thread_shuffle_thread_id": 12345
        }
        mock_lobby_service.get_lobby_thread_id.return_value = 12345

        # Create the cog
        cog = MatchCommands(
            mock_bot,
            mock_lobby_service,
            mock_match_service,
            mock_player_service,
        )

        # Patch asyncio.sleep at the module where it's imported to track calls
        sleep_calls = []

        async def mock_sleep(seconds):
            sleep_calls.append(seconds)
            # Don't actually sleep, just record the call
            return

        # Patch at the commands.match module level to catch any import style
        with patch("commands.match.asyncio.sleep", mock_sleep):
            await cog._abort_lobby_thread(guild_id=123)

        # Verify no long sleeps (>= 5 seconds) were called
        # This catches the original 15-second bug and any similar long delays
        long_sleeps = [s for s in sleep_calls if s >= 5.0]
        assert not long_sleeps, (
            f"_abort_lobby_thread should not have long sleeps (>=5s), "
            f"but found: {long_sleeps}"
        )

        # Verify the thread operations were called
        mock_thread.send.assert_called_once()
        mock_thread.edit.assert_called()

    @pytest.mark.asyncio
    async def test_abort_completes_quickly(self):
        """Verify abort operation completes in under 1 second (not 15+ seconds)."""
        import time
        from commands.match import MatchCommands

        # Create mock bot and services
        mock_bot = MagicMock()
        mock_lobby_service = MagicMock()
        mock_match_service = MagicMock()
        mock_player_service = MagicMock()

        # Setup mock thread
        mock_thread = AsyncMock()
        mock_thread.send = AsyncMock()
        mock_thread.edit = AsyncMock()
        mock_bot.get_channel = MagicMock(return_value=mock_thread)
        mock_bot.fetch_channel = AsyncMock(return_value=mock_thread)

        # Setup pending state
        mock_match_service.get_last_shuffle.return_value = {
            "thread_shuffle_thread_id": 12345
        }
        mock_lobby_service.get_lobby_thread_id.return_value = 12345

        cog = MatchCommands(
            mock_bot,
            mock_lobby_service,
            mock_match_service,
            mock_player_service,
        )

        start_time = time.time()
        await cog._abort_lobby_thread(guild_id=123)
        elapsed = time.time() - start_time

        # Should complete in under 1 second (was 15+ seconds before fix)
        assert elapsed < 1.0, (
            f"_abort_lobby_thread took {elapsed:.2f}s, should complete in under 1s"
        )


class TestGamblingLeaderboardNoFetchUser:
    """
    Regression test for Issue 2C: Gambling leaderboard timeout.

    The leaderboard now pre-fetches guild members instead of making individual
    bot.fetch_user() calls, which caused timeouts with many entries.

    Updated: These tests now verify the UnifiedLeaderboardView's gambling tab
    uses the same guild member caching pattern.
    """

    @pytest.mark.asyncio
    async def test_gambling_leaderboard_uses_guild_members_cache(self):
        """Verify UnifiedLeaderboardView's gambling tab uses pre-fetched guild members."""
        from commands.info import UnifiedLeaderboardView, LeaderboardTab

        # Create mock cog
        mock_cog = MagicMock()
        mock_cog.gambling_stats_service = MagicMock()
        mock_cog.gambling_stats_service.get_leaderboard.return_value = Leaderboard(
            top_earners=[
                LeaderboardEntry(
                    discord_id=1001, total_bets=5, wins=3, losses=2,
                    win_rate=0.6, net_pnl=50, total_wagered=100, avg_leverage=1.5,
                    degen_score=30, degen_title="Recreational", degen_emoji="🎰"
                ),
                LeaderboardEntry(
                    discord_id=1002, total_bets=3, wins=1, losses=2,
                    win_rate=0.33, net_pnl=-20, total_wagered=60, avg_leverage=1.0,
                    degen_score=15, degen_title="Casual", degen_emoji="🥱"
                ),
            ],
            down_bad=[],
            hall_of_degen=[],
            biggest_gamblers=[],
            total_wagered=160,
            total_bets=8,
            avg_degen_score=22.5,
            total_bankruptcies=0,
            total_loans=0,
            server_stats={
                "total_bets": 8,
                "total_wagered": 160,
                "unique_gamblers": 2,
                "avg_bet_size": 20,
                "total_bankruptcies": 0,
            },
        )
        mock_cog.bankruptcy_service = MagicMock()
        mock_cog.bankruptcy_service.get_bulk_states.return_value = {}

        # Create mock interaction with guild members
        mock_member1 = MagicMock()
        mock_member1.id = 1001
        mock_member1.display_name = "CachedUser1"

        mock_member2 = MagicMock()
        mock_member2.id = 1002
        mock_member2.display_name = "CachedUser2"

        mock_guild = MagicMock()
        mock_guild.id = 12345
        mock_guild.members = [mock_member1, mock_member2]

        mock_interaction = MagicMock()
        mock_interaction.guild = mock_guild
        mock_interaction.user = MagicMock()
        mock_interaction.user.id = 1001

        # Create the view
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=12345,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GAMBLING,
        )

        # Load gambling data
        await view._load_tab_data(LeaderboardTab.GAMBLING)

        # Verify guild_members are cached in state.extra
        state = view._tab_states[LeaderboardTab.GAMBLING]
        assert "guild_members" in state.extra
        assert 1001 in state.extra["guild_members"]
        assert 1002 in state.extra["guild_members"]

        # Build embed and verify cached names are used
        embed = view.build_embed()
        embed_text = ""
        for field in embed.fields:
            embed_text += field.name + field.value

        assert "CachedUser1" in embed_text, (
            "Embed should use cached guild member name 'CachedUser1'"
        )

    @pytest.mark.asyncio
    async def test_gambling_leaderboard_handles_missing_guild(self):
        """Verify UnifiedLeaderboardView handles DM context (no guild) gracefully."""
        from commands.info import UnifiedLeaderboardView, LeaderboardTab

        # Create mock cog
        mock_cog = MagicMock()
        mock_cog.gambling_stats_service = MagicMock()
        mock_cog.gambling_stats_service.get_leaderboard.return_value = Leaderboard(
            top_earners=[
                LeaderboardEntry(
                    discord_id=1001, total_bets=5, wins=3, losses=2,
                    win_rate=0.6, net_pnl=50, total_wagered=100, avg_leverage=1.5,
                    degen_score=30, degen_title="Recreational", degen_emoji="🎰"
                ),
            ],
            down_bad=[],
            hall_of_degen=[],
            biggest_gamblers=[],
            total_wagered=100,
            total_bets=5,
            avg_degen_score=30,
            total_bankruptcies=0,
            total_loans=0,
            server_stats={
                "total_bets": 5,
                "total_wagered": 100,
                "unique_gamblers": 1,
                "avg_bet_size": 20,
                "total_bankruptcies": 0,
            },
        )
        mock_cog.bankruptcy_service = MagicMock()
        mock_cog.bankruptcy_service.get_bulk_states.return_value = {}

        # No guild (DM context)
        mock_interaction = MagicMock()
        mock_interaction.guild = None
        mock_interaction.user = MagicMock()
        mock_interaction.user.id = 1001

        # Create the view (should not raise)
        view = UnifiedLeaderboardView(
            cog=mock_cog,
            guild_id=None,
            interaction=mock_interaction,
            initial_tab=LeaderboardTab.GAMBLING,
        )

        # Load gambling data - should not raise
        await view._load_tab_data(LeaderboardTab.GAMBLING)

        # Should have empty guild_members dict
        state = view._tab_states[LeaderboardTab.GAMBLING]
        assert state.extra.get("guild_members") == {}

        # Build embed should work and fall back to "User {id}"
        embed = view.build_embed()
        assert embed is not None
        assert len(embed.fields) > 0 or embed.description is not None


class TestProfileTeammatesSpacerPresent:
    """
    Regression test for Issue 1: Profile embed alignment.

    The spacer field between "Most Played Against" and "Even Teammates" is
    required for proper Discord embed layout (3 inline fields per row).
    """

    @pytest.mark.asyncio
    async def test_teammates_tab_has_spacer_after_most_played_against(self, repositories):
        """Verify the teammates tab includes spacer for proper row layout."""
        from commands.profile import ProfileCommands
        import discord

        # Create mock bot with services attached
        mock_bot = MagicMock()
        mock_bot.player_repo = repositories["player_repo"]
        mock_bot.match_repo = repositories["match_repo"]
        mock_bot.pairings_repo = repositories["pairings_repo"]

        # Create the cog
        cog = ProfileCommands(bot=mock_bot)

        # Create test player
        repositories["player_repo"].add(
            discord_id=1001,
            discord_username="TestPlayer",
            initial_mmr=3000,
        )

        # Create mock user
        mock_user = MagicMock()
        mock_user.id = 1001
        mock_user.display_name = "TestPlayer"
        mock_user.display_avatar = MagicMock()
        mock_user.display_avatar.url = "https://example.com/avatar.png"

        # Call _build_teammates_embed directly
        embed, _ = await cog._build_teammates_embed(
            target_user=mock_user,
            target_discord_id=1001,
        )

        # Count spacer fields (name="\u200b", value="\u200b")
        spacer_count = sum(
            1 for field in embed.fields
            if field.name == "\u200b" and field.value == "\u200b"
        )

        # Should have exactly 3 spacers for proper row layout:
        # Row 1: Best Teammates | Worst Teammates | Spacer (1)
        # Row 2: Dominates | Struggles Against | Spacer (2)
        # Row 3: Most Played With | Most Played Against | Spacer (3)
        # Row 4: Even Teammates | Even Opponents (no spacer needed - last row)
        assert spacer_count == 3, (
            f"Expected exactly 3 spacer fields for proper row layout, found {spacer_count}. "
            "Each pair of inline fields needs a spacer to complete the row."
        )

    @pytest.mark.asyncio
    async def test_teammates_tab_field_order_correct(self, repositories):
        """Verify field order ensures proper inline grouping."""
        from commands.profile import ProfileCommands

        mock_bot = MagicMock()
        mock_bot.player_repo = repositories["player_repo"]
        mock_bot.match_repo = repositories["match_repo"]
        mock_bot.pairings_repo = repositories["pairings_repo"]

        cog = ProfileCommands(bot=mock_bot)

        repositories["player_repo"].add(
            discord_id=1001,
            discord_username="TestPlayer",
            initial_mmr=3000,
        )

        mock_user = MagicMock()
        mock_user.id = 1001
        mock_user.display_name = "TestPlayer"
        mock_user.display_avatar = MagicMock()
        mock_user.display_avatar.url = "https://example.com/avatar.png"

        embed, _ = await cog._build_teammates_embed(
            target_user=mock_user,
            target_discord_id=1001,
        )

        # Get field names (excluding spacers)
        field_names = [f.name for f in embed.fields if f.name != "\u200b"]

        # Verify expected fields exist
        expected_fields = [
            "Best Teammates",
            "Worst Teammates",
            "Dominates",
            "Struggles Against",
            "Most Played With",
            "Most Played Against",
        ]

        for expected in expected_fields:
            # Check if any field name contains the expected text (emoji prefix varies)
            found = any(expected in name for name in field_names)
            assert found, f"Expected field containing '{expected}' not found in {field_names}"


class TestGetNameFunctionSync:
    """
    Verify the get_name helper function is synchronous and uses cached members.

    Updated: Tests now verify the UnifiedLeaderboardView's _get_name_for_gambling
    method is synchronous and uses cached guild members.
    """

    def test_get_name_is_sync_in_gambling_leaderboard(self):
        """Verify _get_name_for_gambling in UnifiedLeaderboardView is a sync function."""
        import inspect
        from commands.info import UnifiedLeaderboardView

        # Get the source code of _get_name_for_gambling
        source = inspect.getsource(UnifiedLeaderboardView._get_name_for_gambling)

        # Check that it's defined as sync (def, not async def)
        assert "def _get_name_for_gambling(self, discord_id: int) -> str:" in source, (
            "_get_name_for_gambling should be a synchronous function (def, not async def)"
        )

        # Verify there's no 'await' in the method
        assert "await" not in source, (
            "_get_name_for_gambling should not use await - it should be synchronous"
        )

        # Verify it uses guild_members from state.extra
        assert "guild_members" in source, (
            "Should use guild_members from state.extra for O(1) lookup"
        )


class TestCriticalCommandsAlwaysLoaded:
    """
    Regression tests to ensure critical commands are always loadable.

    These tests prevent deployment issues where commands don't appear in Discord
    due to import errors, missing dependencies, or misconfiguration.
    """

    def test_critical_extensions_in_extensions_list(self):
        """Verify critical command extensions are in the EXTENSIONS list."""
        from bot import EXTENSIONS

        # Critical extensions that must always be loaded
        critical_extensions = [
            "commands.info",      # /help, /leaderboard, /calibration
            "commands.profile",   # /profile
        ]

        for ext in critical_extensions:
            assert ext in EXTENSIONS, (
                f"Critical extension '{ext}' is missing from EXTENSIONS list in bot.py. "
                "This will cause commands to not appear in Discord."
            )

    def test_info_commands_module_imports_without_error(self):
        """Verify commands.info module can be imported without errors."""
        # This catches missing dependencies like matplotlib, dotabase
        try:
            from commands import info
            assert hasattr(info, "InfoCommands"), "InfoCommands class should exist"
            assert hasattr(info, "setup"), "setup function should exist"
        except ImportError as e:
            pytest.fail(f"commands.info failed to import: {e}")

    def test_profile_commands_module_imports_without_error(self):
        """Verify commands.profile module can be imported without errors."""
        try:
            from commands import profile
            assert hasattr(profile, "ProfileCommands"), "ProfileCommands class should exist"
            assert hasattr(profile, "setup"), "setup function should exist"
        except ImportError as e:
            pytest.fail(f"commands.profile failed to import: {e}")

    def test_dota_info_commands_module_imports_without_error(self):
        """Verify commands.dota_info module can be imported (requires dotabase)."""
        try:
            from commands import dota_info
            assert hasattr(dota_info, "DotaInfoCommands"), "DotaInfoCommands class should exist"
            assert hasattr(dota_info, "setup"), "setup function should exist"
        except ImportError as e:
            pytest.fail(f"commands.dota_info failed to import: {e}")

    def test_info_cog_has_help_command(self):
        """Verify InfoCommands cog has the /help command."""
        from commands.info import InfoCommands
        from discord.app_commands import Command

        # Check that help_command method exists and is decorated as app_command
        assert hasattr(InfoCommands, "help_command"), (
            "InfoCommands should have a help_command method"
        )

        # Decorated methods become Command objects
        cmd = getattr(InfoCommands, "help_command")
        assert isinstance(cmd, Command), (
            "help_command should be a discord.app_commands.Command"
        )
        assert cmd.name == "help", "Command name should be 'help'"

    def test_info_cog_has_leaderboard_command(self):
        """Verify InfoCommands cog has the /leaderboard command."""
        from commands.info import InfoCommands
        from discord.app_commands import Command

        assert hasattr(InfoCommands, "leaderboard"), (
            "InfoCommands should have a leaderboard method"
        )
        cmd = getattr(InfoCommands, "leaderboard")
        assert isinstance(cmd, Command), (
            "leaderboard should be a discord.app_commands.Command"
        )
        assert cmd.name == "leaderboard", "Command name should be 'leaderboard'"

    def test_profile_cog_has_profile_command(self):
        """Verify ProfileCommands cog has the /profile command."""
        from commands.profile import ProfileCommands
        from discord.app_commands import Command

        assert hasattr(ProfileCommands, "profile"), (
            "ProfileCommands should have a profile method"
        )
        cmd = getattr(ProfileCommands, "profile")
        assert isinstance(cmd, Command), (
            "profile should be a discord.app_commands.Command"
        )
        assert cmd.name == "profile", "Command name should be 'profile'"

    def test_dota_info_cog_has_hero_and_ability_commands(self):
        """Verify DotaInfoCommands cog has /hero and /ability commands."""
        from commands.dota_info import DotaInfoCommands
        from discord.app_commands import Command

        assert hasattr(DotaInfoCommands, "hero"), (
            "DotaInfoCommands should have a hero method"
        )
        hero_cmd = getattr(DotaInfoCommands, "hero")
        assert isinstance(hero_cmd, Command), "hero should be a Command"
        assert hero_cmd.name == "hero", "Command name should be 'hero'"

        assert hasattr(DotaInfoCommands, "ability"), (
            "DotaInfoCommands should have an ability method"
        )
        ability_cmd = getattr(DotaInfoCommands, "ability")
        assert isinstance(ability_cmd, Command), "ability should be a Command"
        assert ability_cmd.name == "ability", "Command name should be 'ability'"

    def test_info_cog_can_be_instantiated(self):
        """Verify InfoCommands cog can be instantiated with mock dependencies."""
        from commands.info import InfoCommands

        mock_bot = MagicMock()

        # InfoCommands requires these constructor args
        cog = InfoCommands(
            bot=mock_bot,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
        )

        assert cog is not None, "InfoCommands should instantiate successfully"
        assert cog.bot is mock_bot, "Bot should be stored on cog"

    def test_profile_cog_can_be_instantiated(self):
        """Verify ProfileCommands cog can be instantiated with mock dependencies."""
        from commands.profile import ProfileCommands

        mock_bot = MagicMock()
        cog = ProfileCommands(bot=mock_bot)

        assert cog is not None, "ProfileCommands should instantiate successfully"
        assert cog.bot is mock_bot, "Bot should be stored on cog"

    def test_all_extensions_importable(self):
        """Verify all extensions in EXTENSIONS list can be imported."""
        from bot import EXTENSIONS
        import importlib

        failed_imports = []
        for ext in EXTENSIONS:
            try:
                importlib.import_module(ext)
            except ImportError as e:
                failed_imports.append((ext, str(e)))

        if failed_imports:
            failures = "\n".join(f"  - {ext}: {err}" for ext, err in failed_imports)
            pytest.fail(
                f"The following extensions failed to import:\n{failures}\n"
                "This will cause commands to not appear in Discord."
            )


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
