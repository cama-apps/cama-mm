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
    """

    @pytest.mark.asyncio
    async def test_gambling_leaderboard_uses_guild_members_cache(self):
        """Verify _show_gambling_leaderboard uses pre-fetched guild members."""
        from commands.info import InfoCommands

        # Create mock bot
        mock_bot = MagicMock()
        mock_bot.fetch_user = AsyncMock(return_value=MagicMock(display_name="FetchedUser"))

        # Create mock interaction with guild members
        mock_member1 = MagicMock()
        mock_member1.id = 1001
        mock_member1.display_name = "CachedUser1"

        mock_member2 = MagicMock()
        mock_member2.id = 1002
        mock_member2.display_name = "CachedUser2"

        mock_guild = MagicMock()
        mock_guild.members = [mock_member1, mock_member2]

        mock_interaction = MagicMock()
        mock_interaction.guild = mock_guild
        mock_interaction.followup = AsyncMock()
        mock_interaction.followup.send = AsyncMock()

        # Create mock gambling stats service that returns a leaderboard
        mock_gambling_service = MagicMock()
        mock_gambling_service.get_leaderboard.return_value = Leaderboard(
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

        # Create the cog
        cog = InfoCommands(
            bot=mock_bot,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
            gambling_stats_service=mock_gambling_service,
        )

        # Patch safe_followup to capture the embed
        with patch("commands.info.safe_followup", new_callable=AsyncMock) as mock_followup:
            await cog._show_gambling_leaderboard(mock_interaction, limit=5)

            # Verify bot.fetch_user was NEVER called (we use cached guild members)
            mock_bot.fetch_user.assert_not_called()

            # Verify safe_followup was called with embed
            mock_followup.assert_called_once()
            call_kwargs = mock_followup.call_args[1]
            embed = call_kwargs.get("embed")
            assert embed is not None, "Should have sent an embed"

            # Verify the embed contains the cached usernames (not "FetchedUser")
            embed_text = ""
            for field in embed.fields:
                embed_text += field.name + field.value

            assert "CachedUser1" in embed_text, (
                "Embed should use cached guild member name 'CachedUser1'"
            )
            assert "FetchedUser" not in embed_text, (
                "Embed should not contain 'FetchedUser' - should use cached names"
            )

    @pytest.mark.asyncio
    async def test_gambling_leaderboard_handles_missing_guild(self):
        """Verify leaderboard handles DM context (no guild) gracefully."""
        from commands.info import InfoCommands

        mock_bot = MagicMock()
        mock_bot.fetch_user = AsyncMock()

        # No guild (DM context)
        mock_interaction = MagicMock()
        mock_interaction.guild = None
        mock_interaction.followup = AsyncMock()

        mock_gambling_service = MagicMock()
        mock_gambling_service.get_leaderboard.return_value = Leaderboard(
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

        cog = InfoCommands(
            bot=mock_bot,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
            gambling_stats_service=mock_gambling_service,
        )

        with patch("commands.info.safe_followup", new_callable=AsyncMock) as mock_followup:
            # Should not raise even without guild
            await cog._show_gambling_leaderboard(mock_interaction, limit=5)

            # Still shouldn't call fetch_user (falls back to "User {id}")
            mock_bot.fetch_user.assert_not_called()

            # Should still send embed
            mock_followup.assert_called_once()


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
    """

    def test_get_name_is_sync_in_gambling_leaderboard(self):
        """Verify get_name in _show_gambling_leaderboard is a sync function."""
        import inspect
        from commands.info import InfoCommands

        # Get the source code of _show_gambling_leaderboard
        source = inspect.getsource(InfoCommands._show_gambling_leaderboard)

        # Check that get_name is defined as sync (def, not async def)
        assert "def get_name(discord_id: int) -> str:" in source, (
            "get_name should be a synchronous function (def, not async def)"
        )

        # Verify there's no 'await get_name' in the method
        assert "await get_name" not in source, (
            "get_name should not be awaited - it should be synchronous"
        )

        # Verify guild_members pre-fetch exists
        assert "guild_members = {m.id: m for m in interaction.guild.members}" in source, (
            "Should pre-fetch guild members into a dict for O(1) lookup"
        )


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
