import asyncio
import time
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from repositories.notification_repository import NotificationRepository
from services.reminder_service import ReminderService
from tests.conftest import TEST_GUILD_ID

TEST_GUILD_ID_2 = 99999


@pytest.fixture
def notification_repo(repo_db_path):
    return NotificationRepository(repo_db_path)


@pytest.fixture
def player_repo_mock():
    mock = MagicMock()
    mock.get_last_wheel_spin.return_value = None
    mock.get_last_trivia_session.return_value = None
    return mock


@pytest.fixture
def reminder_service(notification_repo, player_repo_mock):
    return ReminderService(notification_repo=notification_repo, player_repo=player_repo_mock)


@pytest.fixture
def mock_bot():
    bot = MagicMock()
    bot.get_user.return_value = None
    bot.fetch_user = AsyncMock(return_value=MagicMock())
    return bot


# ---------------------------------------------------------------------------
# NotificationRepository
# ---------------------------------------------------------------------------


class TestNotificationRepository:
    def test_defaults_when_no_row(self, notification_repo):
        prefs = notification_repo.get_preferences(9001, TEST_GUILD_ID)
        assert prefs == {"wheel_enabled": False, "trivia_enabled": False, "betting_enabled": False, "dig_enabled": False}

    def test_set_wheel_preference(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "wheel", True)
        prefs = notification_repo.get_preferences(1, TEST_GUILD_ID)
        assert prefs["wheel_enabled"] is True
        assert prefs["trivia_enabled"] is False
        assert prefs["betting_enabled"] is False

    def test_set_preference_idempotent(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "betting", True)
        notification_repo.set_preference(1, TEST_GUILD_ID, "betting", True)
        assert notification_repo.get_preferences(1, TEST_GUILD_ID)["betting_enabled"] is True

    def test_set_preference_toggle_off(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "trivia", True)
        notification_repo.set_preference(1, TEST_GUILD_ID, "trivia", False)
        assert notification_repo.get_preferences(1, TEST_GUILD_ID)["trivia_enabled"] is False

    def test_get_enabled_users_for_type(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "wheel", True)
        notification_repo.set_preference(2, TEST_GUILD_ID, "wheel", False)
        notification_repo.set_preference(3, TEST_GUILD_ID, "wheel", True)
        users = notification_repo.get_enabled_users_for_type(TEST_GUILD_ID, "wheel")
        assert set(users) == {1, 3}

    def test_get_enabled_users_empty(self, notification_repo):
        assert notification_repo.get_enabled_users_for_type(TEST_GUILD_ID, "betting") == []

    def test_invalid_type_raises(self, notification_repo):
        with pytest.raises(ValueError):
            notification_repo.set_preference(1, TEST_GUILD_ID, "invalid", True)

    def test_guild_isolation(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "wheel", True)
        notification_repo.set_preference(1, TEST_GUILD_ID_2, "wheel", False)
        assert notification_repo.get_preferences(1, TEST_GUILD_ID)["wheel_enabled"] is True
        assert notification_repo.get_preferences(1, TEST_GUILD_ID_2)["wheel_enabled"] is False

    def test_guild_id_none_normalized(self, notification_repo):
        notification_repo.set_preference(1, None, "trivia", True)
        assert notification_repo.get_preferences(1, None)["trivia_enabled"] is True
        assert notification_repo.get_preferences(1, 0)["trivia_enabled"] is True


# ---------------------------------------------------------------------------
# ReminderService — preferences
# ---------------------------------------------------------------------------


class TestReminderServicePreferences:
    def test_toggle_off_to_on(self, reminder_service):
        result = reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        assert result is True

    def test_toggle_on_to_off(self, reminder_service):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        result = reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        assert result is False

    def test_get_preferences_proxy(self, reminder_service):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "betting")
        prefs = reminder_service.get_preferences(1, TEST_GUILD_ID)
        assert prefs["betting_enabled"] is True


# ---------------------------------------------------------------------------
# ReminderService — task scheduling
# ---------------------------------------------------------------------------


class TestReminderServiceScheduling:
    def test_no_task_when_pref_disabled(self, reminder_service, mock_bot):
        future_time = int(time.time()) + 3600
        reminder_service.schedule_wheel_reminder(mock_bot, 1, TEST_GUILD_ID, future_time)
        assert (1, TEST_GUILD_ID, "wheel") not in reminder_service._tasks

    @pytest.mark.asyncio
    async def test_task_created_when_pref_enabled(self, reminder_service, mock_bot):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        future_time = int(time.time()) + 3600
        reminder_service.schedule_wheel_reminder(mock_bot, 1, TEST_GUILD_ID, future_time)
        assert (1, TEST_GUILD_ID, "wheel") in reminder_service._tasks
        reminder_service._cancel_task(1, TEST_GUILD_ID, "wheel")

    @pytest.mark.asyncio
    async def test_rescheduling_cancels_old_task(self, reminder_service, mock_bot):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "trivia")
        future_time = int(time.time()) + 3600
        reminder_service.schedule_trivia_reminder(mock_bot, 1, TEST_GUILD_ID, future_time)
        first_task = reminder_service._tasks.get((1, TEST_GUILD_ID, "trivia"))
        reminder_service.schedule_trivia_reminder(mock_bot, 1, TEST_GUILD_ID, future_time + 100)
        second_task = reminder_service._tasks.get((1, TEST_GUILD_ID, "trivia"))
        assert first_task is not second_task
        await asyncio.sleep(0)  # allow cancellation to finalize
        assert first_task.cancelled()
        reminder_service._cancel_task(1, TEST_GUILD_ID, "trivia")

    @pytest.mark.asyncio
    async def test_disabling_cancels_task(self, reminder_service, mock_bot):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        future_time = int(time.time()) + 3600
        reminder_service.schedule_wheel_reminder(mock_bot, 1, TEST_GUILD_ID, future_time)
        task = reminder_service._tasks[(1, TEST_GUILD_ID, "wheel")]
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")  # disable
        await asyncio.sleep(0)  # allow cancellation to finalize
        assert task.cancelled()
        assert (1, TEST_GUILD_ID, "wheel") not in reminder_service._tasks


# ---------------------------------------------------------------------------
# ReminderService — restart recovery
# ---------------------------------------------------------------------------


class TestReminderServiceRestartRecovery:
    @pytest.mark.asyncio
    async def test_reschedule_skips_expired_cooldown(
        self, reminder_service, player_repo_mock, mock_bot
    ):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        # last spin was long ago — cooldown already expired
        player_repo_mock.get_last_wheel_spin.return_value = int(time.time()) - 200000
        await reminder_service.reschedule_all(mock_bot, [TEST_GUILD_ID])
        assert (1, TEST_GUILD_ID, "wheel") not in reminder_service._tasks

    @pytest.mark.asyncio
    async def test_reschedule_creates_task_for_active_cooldown(
        self, reminder_service, player_repo_mock, mock_bot
    ):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        # last spin was recent — cooldown still active
        player_repo_mock.get_last_wheel_spin.return_value = int(time.time()) - 100
        await reminder_service.reschedule_all(mock_bot, [TEST_GUILD_ID])
        assert (1, TEST_GUILD_ID, "wheel") in reminder_service._tasks
        reminder_service._cancel_task(1, TEST_GUILD_ID, "wheel")

    @pytest.mark.asyncio
    async def test_reschedule_skips_none_last_spin(
        self, reminder_service, player_repo_mock, mock_bot
    ):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        player_repo_mock.get_last_wheel_spin.return_value = None
        await reminder_service.reschedule_all(mock_bot, [TEST_GUILD_ID])
        assert (1, TEST_GUILD_ID, "wheel") not in reminder_service._tasks


# ---------------------------------------------------------------------------
# ReminderService — betting subscribers
# ---------------------------------------------------------------------------


class TestReminderServiceBetting:
    @pytest.mark.asyncio
    async def test_notify_betting_no_subscribers(self, reminder_service, mock_bot):
        # No-op when nobody subscribed — just verify it doesn't error
        await reminder_service.notify_betting_subscribers(mock_bot, TEST_GUILD_ID, int(time.time()) + 900)

    @pytest.mark.asyncio
    async def test_notify_betting_dms_subscribers(self, reminder_service, mock_bot):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "betting")
        reminder_service.toggle_preference(2, TEST_GUILD_ID, "betting")

        sent_to = []

        async def fake_dm(bot, discord_id, message):
            sent_to.append(discord_id)

        with patch.object(reminder_service, "_dm_user", new=fake_dm):
            await reminder_service.notify_betting_subscribers(
                mock_bot, TEST_GUILD_ID, int(time.time()) + 900
            )
            # Yield to the event loop so the fire-and-forget tasks run
            for _ in range(5):
                await asyncio.sleep(0)

        assert set(sent_to) == {1, 2}


# ---------------------------------------------------------------------------
# Dig reminder
# ---------------------------------------------------------------------------


@pytest.fixture
def dig_repo_mock():
    mock = MagicMock()
    mock.get_tunnel.return_value = None
    return mock


@pytest.fixture
def reminder_service_with_dig(notification_repo, player_repo_mock, dig_repo_mock):
    return ReminderService(
        notification_repo=notification_repo,
        player_repo=player_repo_mock,
        dig_repo=dig_repo_mock,
    )


class TestDigReminder:
    def test_dig_preference_default_false(self, notification_repo):
        prefs = notification_repo.get_preferences(1, TEST_GUILD_ID)
        assert prefs.get("dig_enabled") is False

    def test_dig_preference_toggle(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "dig", True)
        assert notification_repo.get_preferences(1, TEST_GUILD_ID)["dig_enabled"] is True

    @pytest.mark.asyncio
    async def test_dig_schedule_creates_task_when_enabled(
        self, reminder_service_with_dig, mock_bot
    ):
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        future_time = int(time.time()) + 3600
        reminder_service_with_dig.schedule_dig_reminder(mock_bot, 1, TEST_GUILD_ID, future_time)
        assert (1, TEST_GUILD_ID, "dig") in reminder_service_with_dig._tasks
        reminder_service_with_dig._cancel_task(1, TEST_GUILD_ID, "dig")

    def test_dig_no_task_when_disabled(self, reminder_service_with_dig, mock_bot):
        future_time = int(time.time()) + 3600
        reminder_service_with_dig.schedule_dig_reminder(mock_bot, 1, TEST_GUILD_ID, future_time)
        assert (1, TEST_GUILD_ID, "dig") not in reminder_service_with_dig._tasks

    @pytest.mark.asyncio
    async def test_reschedule_dig_active_cooldown(
        self, reminder_service_with_dig, dig_repo_mock, mock_bot
    ):
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_repo_mock.get_tunnel.return_value = {"last_dig_at": int(time.time()) - 100}
        await reminder_service_with_dig.reschedule_all(mock_bot, [TEST_GUILD_ID])
        assert (1, TEST_GUILD_ID, "dig") in reminder_service_with_dig._tasks
        reminder_service_with_dig._cancel_task(1, TEST_GUILD_ID, "dig")

    @pytest.mark.asyncio
    async def test_reschedule_dig_skips_none_tunnel(
        self, reminder_service_with_dig, dig_repo_mock, mock_bot
    ):
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_repo_mock.get_tunnel.return_value = None
        await reminder_service_with_dig.reschedule_all(mock_bot, [TEST_GUILD_ID])
        assert (1, TEST_GUILD_ID, "dig") not in reminder_service_with_dig._tasks

    @pytest.mark.asyncio
    async def test_reschedule_dig_skips_expired_cooldown(
        self, reminder_service_with_dig, dig_repo_mock, mock_bot
    ):
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_repo_mock.get_tunnel.return_value = {"last_dig_at": int(time.time()) - 200000}
        await reminder_service_with_dig.reschedule_all(mock_bot, [TEST_GUILD_ID])
        assert (1, TEST_GUILD_ID, "dig") not in reminder_service_with_dig._tasks
