"""Tests for continuous real-time activity monitoring (/rc command)."""

import asyncio
from datetime import datetime
from unittest.mock import AsyncMock, MagicMock, Mock, patch

import discord
import pytest

from commands.lobby import LobbyCommands
from domain.models.player import Player
from services.afk_detection_service import ActivityStatus


@pytest.fixture
def mock_bot():
    """Create a mock Discord bot."""
    bot = Mock(spec=discord.Client)
    bot.get_channel = Mock(return_value=None)
    bot.fetch_channel = AsyncMock(return_value=None)
    return bot


@pytest.fixture
def mock_lobby_service():
    """Create a mock LobbyService."""
    service = Mock()
    service.get_lobby = Mock()
    service.get_lobby_players = Mock()
    service.get_lobby_message_id = Mock(return_value=12345)
    service.get_lobby_thread_id = Mock(return_value=67890)
    return service


@pytest.fixture
def mock_player_service():
    """Create a mock PlayerService."""
    return Mock()


@pytest.fixture
def lobby_commands(mock_bot, mock_lobby_service, mock_player_service):
    """Create LobbyCommands instance with mocks."""
    return LobbyCommands(mock_bot, mock_lobby_service, mock_player_service)


@pytest.fixture
def sample_players():
    """Create sample players for testing."""
    return [
        Player(
            name="Player1",
            mmr=3000,
            discord_id=111,
            preferred_roles=["1", "2"],
            main_role="1",
        ),
        Player(
            name="Player2",
            mmr=2800,
            discord_id=222,
            preferred_roles=["2", "3"],
            main_role="2",
        ),
        Player(
            name="Player3",
            mmr=2600,
            discord_id=333,
            preferred_roles=["3", "4"],
            main_role="3",
        ),
    ]


@pytest.fixture
def mock_guild():
    """Create a mock Discord guild."""
    guild = Mock(spec=discord.Guild)
    guild.id = 999
    guild.get_member = Mock(return_value=None)
    return guild


class TestTaskManagement:
    """Tests for RC task registration and cancellation."""

    def test_register_rc_task(self, lobby_commands):
        """Test that RC tasks are registered correctly."""
        guild_id = 123
        task = Mock()
        task.done = Mock(return_value=False)

        lobby_commands._register_rc_task(guild_id, task)

        assert 123 in lobby_commands._rc_tasks
        assert lobby_commands._rc_tasks[123] == task

    def test_register_rc_task_normalizes_none_guild_id(self, lobby_commands):
        """Test that None guild_id is normalized to 0."""
        task = Mock()
        task.done = Mock(return_value=False)

        lobby_commands._register_rc_task(None, task)

        assert 0 in lobby_commands._rc_tasks
        assert lobby_commands._rc_tasks[0] == task

    def test_register_rc_task_cancels_previous(self, lobby_commands):
        """Test that registering a new task cancels the previous one."""
        guild_id = 123
        old_task = Mock()
        old_task.done = Mock(return_value=False)
        old_task.cancel = Mock()

        new_task = Mock()
        new_task.done = Mock(return_value=False)

        # Register first task
        lobby_commands._register_rc_task(guild_id, old_task)
        assert lobby_commands._rc_tasks[123] == old_task

        # Register second task (should cancel first)
        lobby_commands._register_rc_task(guild_id, new_task)

        old_task.cancel.assert_called_once()
        assert lobby_commands._rc_tasks[123] == new_task

    def test_cancel_rc_task(self, lobby_commands):
        """Test that RC tasks can be cancelled."""
        guild_id = 123
        task = Mock()
        task.done = Mock(return_value=False)
        task.cancel = Mock()

        lobby_commands._rc_tasks[123] = task
        lobby_commands._cancel_rc_task(guild_id)

        task.cancel.assert_called_once()
        assert 123 not in lobby_commands._rc_tasks

    def test_cancel_rc_task_skips_done_tasks(self, lobby_commands):
        """Test that completed tasks are not cancelled."""
        guild_id = 123
        task = Mock()
        task.done = Mock(return_value=True)
        task.cancel = Mock()

        lobby_commands._rc_tasks[123] = task
        lobby_commands._cancel_rc_task(guild_id)

        task.cancel.assert_not_called()
        assert 123 not in lobby_commands._rc_tasks

    def test_cancel_rc_task_nonexistent(self, lobby_commands):
        """Test that canceling a nonexistent task doesn't raise error."""
        # Should not raise
        lobby_commands._cancel_rc_task(999)


class TestLiveActivityEmbed:
    """Tests for live activity embed builder."""

    def test_build_initial_embed(self, lobby_commands, sample_players, mock_guild):
        """Test building the initial 'Starting...' embed."""
        embed = lobby_commands._build_live_activity_embed(
            activity_results={},
            players=sample_players,
            player_ids=[111, 222, 333],
            guild=mock_guild,
            time_remaining=300,
            is_initial=True,
        )

        assert embed.title == "🔄 Activity Monitor Starting..."
        assert "300s" in embed.description
        assert "Updates every 5s" in embed.description
        assert embed.color == discord.Color.blue()
        assert "🟢 online" in embed.footer.text

    def test_build_embed_all_active(self, lobby_commands, sample_players, mock_guild):
        """Test building embed when all players are active."""
        activity_results = {
            111: ActivityStatus(
                discord_id=111,
                is_active=True,
                signals=["online", "voice"],
                last_activity_time=datetime.now(),
            ),
            222: ActivityStatus(
                discord_id=222,
                is_active=True,
                signals=["online", "recent_message"],
                last_activity_time=datetime.now(),
            ),
            333: ActivityStatus(
                discord_id=333,
                is_active=True,
                signals=["voice"],
                last_activity_time=datetime.now(),
            ),
        }

        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=sample_players,
            player_ids=[111, 222, 333],
            guild=mock_guild,
            time_remaining=180,
            is_initial=False,
        )

        assert "✅ All Active" in embed.title
        assert "3:00" in embed.title
        assert "3/3 players active" in embed.description
        assert embed.color == discord.Color.green()
        assert len(embed.fields) == 1
        assert embed.fields[0].name == "✅ Active (3)"

    def test_build_embed_some_afk(self, lobby_commands, sample_players, mock_guild):
        """Test building embed when some players are AFK."""
        activity_results = {
            111: ActivityStatus(
                discord_id=111,
                is_active=True,
                signals=["online"],
                last_activity_time=datetime.now(),
            ),
            222: ActivityStatus(
                discord_id=222,
                is_active=False,
                signals=[],
                last_activity_time=None,
            ),
            333: ActivityStatus(
                discord_id=333,
                is_active=True,
                signals=["voice", "recent_message"],
                last_activity_time=datetime.now(),
            ),
        }

        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=sample_players,
            player_ids=[111, 222, 333],
            guild=mock_guild,
            time_remaining=120,
            is_initial=False,
        )

        assert "📊 Activity Monitor" in embed.title
        assert "2:00" in embed.title
        assert "2/3 players active" in embed.description
        assert embed.color == discord.Color.orange()
        assert len(embed.fields) == 2
        assert embed.fields[0].name == "✅ Active (2)"
        assert embed.fields[1].name == "⚠️ No Activity (1)"

    def test_build_embed_countdown_formatting(self, lobby_commands, sample_players, mock_guild):
        """Test countdown timer formatting for various durations."""
        activity_results = {
            111: ActivityStatus(111, True, ["online"], datetime.now()),
        }

        # Test minutes:seconds format
        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=sample_players[:1],
            player_ids=[111],
            guild=mock_guild,
            time_remaining=185,
            is_initial=False,
        )
        assert "3:05" in embed.title

        # Test seconds only format
        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=sample_players[:1],
            player_ids=[111],
            guild=mock_guild,
            time_remaining=45,
            is_initial=False,
        )
        assert "45s" in embed.title

    def test_build_embed_complete(self, lobby_commands, sample_players, mock_guild):
        """Test building final 'Complete' embed."""
        activity_results = {
            111: ActivityStatus(111, True, ["online"], datetime.now()),
        }

        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=sample_players[:1],
            player_ids=[111],
            guild=mock_guild,
            time_remaining=0,
            is_initial=False,
        )

        assert embed.title == "✅ Activity Monitoring Complete"
        assert embed.color == discord.Color.green()

    def test_build_embed_with_afk_service_formatting(self, lobby_commands, sample_players, mock_guild):
        """Test that embed uses AFK service formatting when available."""
        mock_afk_service = Mock()
        mock_afk_service.format_activity_status = Mock(return_value="🟢 🎙️")
        lobby_commands.bot.afk_detection_service = mock_afk_service

        activity_results = {
            111: ActivityStatus(111, True, ["online", "voice"], datetime.now()),
        }

        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=sample_players[:1],
            player_ids=[111],
            guild=mock_guild,
            time_remaining=100,
            is_initial=False,
        )

        # Should have called format_activity_status
        mock_afk_service.format_activity_status.assert_called()
        assert "🟢 🎙️" in embed.fields[0].value


class TestContinuousMonitoring:
    """Tests for continuous monitoring coroutine."""

    @pytest.mark.asyncio
    async def test_monitoring_posts_initial_embed(self, lobby_commands, sample_players, mock_guild):
        """Test that monitoring posts an initial embed."""
        mock_thread = AsyncMock(spec=discord.Thread)
        mock_message = Mock(spec=discord.Message)
        mock_message.edit = AsyncMock()
        mock_thread.send = AsyncMock(return_value=mock_message)

        mock_afk_service = Mock()
        mock_afk_service.check_player_activity = AsyncMock(
            return_value=ActivityStatus(111, True, ["online"], datetime.now())
        )
        lobby_commands.bot.afk_detection_service = mock_afk_service

        # Mock time.time to control loop
        with patch("time.time") as mock_time:
            # Set up time to expire immediately
            mock_time.side_effect = [1000, 1001]  # Start time, then already expired

            await lobby_commands._run_continuous_activity_monitoring(
                guild_id=999,
                guild=mock_guild,
                player_ids=[111],
                players=sample_players[:1],
                lobby_message_id=12345,
                lobby_thread=mock_thread,
                duration_seconds=1,
                refresh_interval=5,
            )

        # Should have posted initial embed
        mock_thread.send.assert_called_once()
        call_args = mock_thread.send.call_args
        embed = call_args.kwargs["embed"]
        assert "Activity Monitor Starting" in embed.title

    @pytest.mark.asyncio
    async def test_monitoring_updates_embed_periodically(self, lobby_commands, sample_players, mock_guild):
        """Test that monitoring updates the embed every refresh interval."""
        mock_thread = AsyncMock(spec=discord.Thread)
        mock_message = Mock(spec=discord.Message)
        mock_message.edit = AsyncMock()
        mock_thread.send = AsyncMock(return_value=mock_message)

        mock_afk_service = Mock()
        mock_afk_service.check_player_activity = AsyncMock(
            return_value=ActivityStatus(111, True, ["online"], datetime.now())
        )
        lobby_commands.bot.afk_detection_service = mock_afk_service

        sleep_calls = 0

        async def mock_sleep(duration):
            nonlocal sleep_calls
            sleep_calls += 1
            if sleep_calls >= 2:  # Allow 2 iterations
                raise asyncio.CancelledError()

        with patch("time.time") as mock_time, patch("asyncio.sleep", side_effect=mock_sleep):
            # Set up time progression
            mock_time.side_effect = [1000, 1000, 1005, 1005]  # Start, loop1, loop2, exit

            try:
                await lobby_commands._run_continuous_activity_monitoring(
                    guild_id=999,
                    guild=mock_guild,
                    player_ids=[111],
                    players=sample_players[:1],
                    lobby_message_id=12345,
                    lobby_thread=mock_thread,
                    duration_seconds=10,
                    refresh_interval=5,
                )
            except asyncio.CancelledError:
                pass

        # Should have edited message at least once
        assert mock_message.edit.call_count >= 1

    @pytest.mark.asyncio
    async def test_monitoring_cleans_up_task_on_completion(self, lobby_commands, sample_players, mock_guild):
        """Test that monitoring removes task from registry on completion."""
        mock_thread = AsyncMock(spec=discord.Thread)
        mock_message = Mock(spec=discord.Message)
        mock_message.edit = AsyncMock()
        mock_thread.send = AsyncMock(return_value=mock_message)

        mock_afk_service = Mock()
        mock_afk_service.check_player_activity = AsyncMock(
            return_value=ActivityStatus(111, True, ["online"], datetime.now())
        )
        lobby_commands.bot.afk_detection_service = mock_afk_service

        guild_id = 999
        lobby_commands._rc_tasks[guild_id] = Mock()

        with patch("time.time") as mock_time:
            mock_time.side_effect = [1000, 1001]  # Expire immediately

            await lobby_commands._run_continuous_activity_monitoring(
                guild_id=guild_id,
                guild=mock_guild,
                player_ids=[111],
                players=sample_players[:1],
                lobby_message_id=12345,
                lobby_thread=mock_thread,
                duration_seconds=1,
                refresh_interval=5,
            )

        # Task should be removed from registry
        assert guild_id not in lobby_commands._rc_tasks

    @pytest.mark.asyncio
    async def test_monitoring_handles_cancellation(self, lobby_commands, sample_players, mock_guild):
        """Test that monitoring handles cancellation gracefully."""
        mock_thread = AsyncMock(spec=discord.Thread)
        mock_message = Mock(spec=discord.Message)
        mock_message.edit = AsyncMock()
        mock_thread.send = AsyncMock(return_value=mock_message)

        mock_afk_service = Mock()
        lobby_commands.bot.afk_detection_service = mock_afk_service

        async def mock_sleep(duration):
            raise asyncio.CancelledError()

        with patch("asyncio.sleep", side_effect=mock_sleep):
            # Should not raise
            await lobby_commands._run_continuous_activity_monitoring(
                guild_id=999,
                guild=mock_guild,
                player_ids=[111],
                players=sample_players[:1],
                lobby_message_id=12345,
                lobby_thread=mock_thread,
                duration_seconds=10,
                refresh_interval=5,
            )

    @pytest.mark.asyncio
    async def test_monitoring_handles_missing_afk_service(self, lobby_commands, sample_players, mock_guild):
        """Test that monitoring handles missing AFK service gracefully."""
        mock_thread = AsyncMock(spec=discord.Thread)
        lobby_commands.bot.afk_detection_service = None

        # Should return early without error
        await lobby_commands._run_continuous_activity_monitoring(
            guild_id=999,
            guild=mock_guild,
            player_ids=[111],
            players=sample_players[:1],
            lobby_message_id=12345,
            lobby_thread=mock_thread,
            duration_seconds=10,
            refresh_interval=5,
        )

        # Should not have attempted to send anything
        mock_thread.send.assert_not_called()


class TestEmbedEdgeCase:
    """Test edge cases for embed building."""

    def test_build_embed_with_no_players(self, lobby_commands, mock_guild):
        """Test building embed with no players."""
        embed = lobby_commands._build_live_activity_embed(
            activity_results={},
            players=[],
            player_ids=[],
            guild=mock_guild,
            time_remaining=60,
            is_initial=False,
        )

        assert "0/0 players active" in embed.description

    def test_build_embed_with_25_plus_players(self, lobby_commands, mock_guild):
        """Test that embed respects Discord's 25 field limit."""
        # Create 30 players
        players = [
            Player(f"Player{i}", 3000, i, ["1"], "1")
            for i in range(30)
        ]
        player_ids = list(range(30))
        activity_results = {
            i: ActivityStatus(i, True, ["online"], datetime.now())
            for i in range(30)
        }

        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=players,
            player_ids=player_ids,
            guild=mock_guild,
            time_remaining=60,
            is_initial=False,
        )

        # Should only include first 25 players (Discord limit)
        assert len(embed.fields) == 1
        # Count lines in field value (should be max 25)
        lines = embed.fields[0].value.split("\n")
        assert len(lines) <= 25

    def test_build_embed_with_none_guild(self, lobby_commands, sample_players):
        """Test building embed with None guild (DM context)."""
        activity_results = {
            111: ActivityStatus(111, True, ["online"], datetime.now()),
        }

        # Should not raise
        embed = lobby_commands._build_live_activity_embed(
            activity_results=activity_results,
            players=sample_players[:1],
            player_ids=[111],
            guild=None,
            time_remaining=60,
            is_initial=False,
        )

        assert embed is not None
        assert embed.title is not None
