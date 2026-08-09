import asyncio
import logging
import threading
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from domain.models.lobby import LobbyKind
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

    def get_reminder_timestamps_bulk(discord_ids, _guild_id):
        return {
            discord_id: {
                "last_wheel_spin": mock.get_last_wheel_spin.return_value,
                "last_trivia_session": mock.get_last_trivia_session.return_value,
            }
            for discord_id in discord_ids
        }

    mock.get_reminder_timestamps_bulk.side_effect = get_reminder_timestamps_bulk
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
        assert prefs == {
            "wheel_enabled": False,
            "trivia_enabled": False,
            "betting_enabled": False,
            "dig_enabled": False,
            "lobby_enabled": False,
            "pet_enabled": False,
        }

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

    def test_set_lobby_preference_and_load_subscribers(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "lobby", True)
        notification_repo.set_preference(2, TEST_GUILD_ID, "lobby", False)
        notification_repo.set_preference(3, TEST_GUILD_ID_2, "lobby", True)

        assert notification_repo.get_preferences(1, TEST_GUILD_ID)["lobby_enabled"] is True
        assert notification_repo.get_enabled_users_for_type(TEST_GUILD_ID, "lobby") == [1]

    def test_get_enabled_users_for_type(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "wheel", True)
        notification_repo.set_preference(2, TEST_GUILD_ID, "wheel", False)
        notification_repo.set_preference(3, TEST_GUILD_ID, "wheel", True)
        users = notification_repo.get_enabled_users_for_type(TEST_GUILD_ID, "wheel")
        assert set(users) == {1, 3}

    def test_get_enabled_users_empty(self, notification_repo):
        assert notification_repo.get_enabled_users_for_type(TEST_GUILD_ID, "betting") == []

    def test_get_enabled_users_by_type_bulk(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "wheel", True)
        notification_repo.set_preference(1, TEST_GUILD_ID, "dig", True)
        notification_repo.set_preference(2, TEST_GUILD_ID, "trivia", True)
        notification_repo.set_preference(3, TEST_GUILD_ID + 1, "wheel", True)

        subscribers = notification_repo.get_enabled_users_by_type_bulk(
            TEST_GUILD_ID,
            ("wheel", "trivia", "dig", "invalid", "wheel"),
        )

        assert subscribers == {
            "wheel": [1],
            "trivia": [2],
            "dig": [1],
        }

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

    def test_lobby_target_subscription_is_idempotent_and_claimed_once(
        self,
        notification_repo,
    ):
        assert notification_repo.add_lobby_target_subscription(
            1,
            10,
            TEST_GUILD_ID,
        ) is True
        assert notification_repo.add_lobby_target_subscription(
            1,
            10,
            TEST_GUILD_ID,
        ) is False

        assert notification_repo.claim_lobby_target_subscribers(
            10,
            TEST_GUILD_ID,
        ) == [1]
        assert notification_repo.claim_lobby_target_subscribers(
            10,
            TEST_GUILD_ID,
        ) == []

        # Player-target DMs are deliberately separate from persistent public
        # 8/9-player rally mentions.
        assert notification_repo.get_preferences(1, TEST_GUILD_ID)[
            "lobby_enabled"
        ] is False
        assert notification_repo.get_enabled_users_for_type(
            TEST_GUILD_ID,
            "lobby",
        ) == []

    def test_lobby_target_subscriptions_support_many_targets_and_subscribers(
        self,
        notification_repo,
    ):
        assert notification_repo.add_lobby_target_subscription(
            1,
            10,
            TEST_GUILD_ID,
        ) is True
        assert notification_repo.add_lobby_target_subscription(
            1,
            11,
            TEST_GUILD_ID,
        ) is True
        assert notification_repo.add_lobby_target_subscription(
            2,
            10,
            TEST_GUILD_ID,
        ) is True

        assert set(
            notification_repo.claim_lobby_target_subscribers(
                10,
                TEST_GUILD_ID,
            )
        ) == {1, 2}
        assert notification_repo.claim_lobby_target_subscribers(
            11,
            TEST_GUILD_ID,
        ) == [1]

    def test_lobby_target_subscription_claim_is_guild_scoped(
        self,
        notification_repo,
    ):
        notification_repo.add_lobby_target_subscription(1, 10, TEST_GUILD_ID)
        notification_repo.add_lobby_target_subscription(1, 10, TEST_GUILD_ID_2)

        assert notification_repo.claim_lobby_target_subscribers(
            10,
            TEST_GUILD_ID,
        ) == [1]
        assert notification_repo.claim_lobby_target_subscribers(
            10,
            TEST_GUILD_ID,
        ) == []
        assert notification_repo.claim_lobby_target_subscribers(
            10,
            TEST_GUILD_ID_2,
        ) == [1]

    def test_has_lobby_target_subscription_reflects_add_and_claim(
        self,
        notification_repo,
    ):
        assert notification_repo.has_lobby_target_subscription(
            1,
            10,
            TEST_GUILD_ID,
        ) is False

        notification_repo.add_lobby_target_subscription(1, 10, TEST_GUILD_ID)

        assert notification_repo.has_lobby_target_subscription(
            1,
            10,
            TEST_GUILD_ID,
        ) is True
        # Scoped to the exact (subscriber, target, guild) triple.
        assert notification_repo.has_lobby_target_subscription(
            2,
            10,
            TEST_GUILD_ID,
        ) is False
        assert notification_repo.has_lobby_target_subscription(
            1,
            11,
            TEST_GUILD_ID,
        ) is False
        assert notification_repo.has_lobby_target_subscription(
            1,
            10,
            TEST_GUILD_ID_2,
        ) is False

        notification_repo.claim_lobby_target_subscribers(10, TEST_GUILD_ID)

        assert notification_repo.has_lobby_target_subscription(
            1,
            10,
            TEST_GUILD_ID,
        ) is False

    def test_service_pass_through_reads_target_subscription(
        self,
        reminder_service,
        notification_repo,
    ):
        assert reminder_service.has_lobby_player_subscription(1, 10, TEST_GUILD_ID) is False
        notification_repo.add_lobby_target_subscription(1, 10, TEST_GUILD_ID)
        assert reminder_service.has_lobby_player_subscription(1, 10, TEST_GUILD_ID) is True

    def test_join_cutoff_leaves_subscriptions_created_after_the_join(
        self,
        notification_repo,
    ):
        """A background claim cannot consume a watch armed after its join."""
        with patch(
            "repositories.notification_repository.time.time_ns",
            side_effect=[100, 200],
        ):
            notification_repo.add_lobby_target_subscription(1, 10, TEST_GUILD_ID)
            notification_repo.add_lobby_target_subscription(2, 10, TEST_GUILD_ID)

        assert notification_repo.claim_lobby_target_subscribers(
            10,
            TEST_GUILD_ID,
            created_at_cutoff=150,
        ) == [1]
        assert notification_repo.claim_lobby_target_subscribers(
            10,
            TEST_GUILD_ID,
            created_at_cutoff=300,
        ) == [2]


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

    def test_set_and_load_lobby_preference(self, reminder_service):
        assert reminder_service.set_preference(1, TEST_GUILD_ID, "lobby", True) is True
        assert reminder_service.get_lobby_subscriber_ids(TEST_GUILD_ID) == [1]

        assert reminder_service.set_preference(1, TEST_GUILD_ID, "lobby", False) is False
        assert reminder_service.get_lobby_subscriber_ids(TEST_GUILD_ID) == []

    def test_lowskill_lobby_subscribers_require_rating_below_1400(
        self,
        reminder_service,
        notification_repo,
        player_repo_mock,
    ):
        for discord_id in (1, 2, 3, 4):
            notification_repo.set_preference(discord_id, TEST_GUILD_ID, "lobby", True)
        player_repo_mock.get_by_ids.return_value = [
            SimpleNamespace(discord_id=1, glicko_rating=1399.0),
            SimpleNamespace(discord_id=2, glicko_rating=1400.0),
            SimpleNamespace(discord_id=3, glicko_rating=1401.0),
        ]

        assert reminder_service.get_lobby_subscriber_ids(
            TEST_GUILD_ID,
            LobbyKind.LOWSKILL,
        ) == [1]


class TestLobbyPlayerNotifications:
    @pytest.mark.asyncio
    async def test_successful_delivery_consumes_subscription_once(
        self,
        reminder_service,
        mock_bot,
    ):
        subscriber = MagicMock(id=1)
        subscriber.send = AsyncMock()
        mock_bot.get_user.return_value = subscriber
        assert reminder_service.add_lobby_player_subscription(
            1,
            10,
            TEST_GUILD_ID,
        ) is True

        attempted = await reminder_service.notify_lobby_player_subscribers(
            mock_bot,
            target_id=10,
            target_display_name="Target Player",
            guild_id=TEST_GUILD_ID,
            lobby_kind=LobbyKind.LOWSKILL,
        )
        repeated = await reminder_service.notify_lobby_player_subscribers(
            mock_bot,
            target_id=10,
            target_display_name="Target Player",
            guild_id=TEST_GUILD_ID,
            lobby_kind=LobbyKind.LOWSKILL,
        )

        assert attempted == 1
        assert repeated == 0
        subscriber.send.assert_awaited_once()
        message = subscriber.send.await_args.args[0]
        assert "Target Player" in message
        assert LobbyKind.LOWSKILL.label in message
        assert "one-time" in message

    @pytest.mark.asyncio
    async def test_target_display_name_is_escaped_in_runtime_dm(
        self,
        reminder_service,
        mock_bot,
    ):
        subscriber = MagicMock(id=1)
        subscriber.send = AsyncMock()
        mock_bot.get_user.return_value = subscriber
        reminder_service.add_lobby_player_subscription(
            1,
            10,
            TEST_GUILD_ID,
        )
        crafted_name = "**<@123> @everyone [click](https://example.invalid)**"

        await reminder_service.notify_lobby_player_subscribers(
            mock_bot,
            target_id=10,
            target_display_name=crafted_name,
            guild_id=TEST_GUILD_ID,
        )

        subscriber.send.assert_awaited_once()
        message = subscriber.send.await_args.args[0]
        assert crafted_name not in message
        assert "<@123>" not in message
        assert "@everyone" not in message
        assert "<@\u200b123\\>" in message
        assert "@\u200beveryone" in message
        assert "\\*\\*" in message
        assert "\\[click\\]\\(" in message

    @pytest.mark.asyncio
    async def test_failed_delivery_still_consumes_subscription(
        self,
        reminder_service,
        mock_bot,
    ):
        subscriber = MagicMock(id=1)
        response = MagicMock(status=403, reason="Forbidden")
        subscriber.send = AsyncMock(
            side_effect=discord.Forbidden(response, "Cannot send messages to this user")
        )
        mock_bot.get_user.return_value = subscriber
        reminder_service.add_lobby_player_subscription(
            1,
            10,
            TEST_GUILD_ID,
        )

        attempted = await reminder_service.notify_lobby_player_subscribers(
            mock_bot,
            target_id=10,
            target_display_name="Target Player",
            guild_id=TEST_GUILD_ID,
        )
        repeated = await reminder_service.notify_lobby_player_subscribers(
            mock_bot,
            target_id=10,
            target_display_name="Target Player",
            guild_id=TEST_GUILD_ID,
        )

        assert attempted == 1
        assert repeated == 0
        subscriber.send.assert_awaited_once()


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
    async def test_finished_task_removed_from_tasks(self, reminder_service, mock_bot):
        """A naturally-completed reminder task must not linger in ``_tasks``.

        Only ``_cancel_task`` used to pop; tasks that ran to completion leaked.
        """
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "wheel")
        # next_spin_time in the past -> delay clamps to 0, task finishes immediately
        reminder_service.schedule_wheel_reminder(mock_bot, 1, TEST_GUILD_ID, int(time.time()) - 10)
        task = reminder_service._tasks.get((1, TEST_GUILD_ID, "wheel"))
        assert task is not None
        await task
        # done-callback runs as a scheduled callback; yield so it fires
        await asyncio.sleep(0)
        assert (1, TEST_GUILD_ID, "wheel") not in reminder_service._tasks

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


    @pytest.mark.asyncio
    async def test_notify_betting_identifies_lobby_and_pending_match(
        self,
        reminder_service,
        mock_bot,
    ):
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "betting")
        sent_messages = []

        async def fake_dm(bot, discord_id, message):
            sent_messages.append(message)

        with patch.object(reminder_service, "_dm_user", new=fake_dm):
            await reminder_service.notify_betting_subscribers(
                mock_bot,
                TEST_GUILD_ID,
                int(time.time()) + 900,
                pending_match_id=42,
                lobby_kind=LobbyKind.LOWSKILL,
            )
            for _ in range(5):
                await asyncio.sleep(0)

        assert sent_messages == [
            "A new 🧀 Whine & Cheese match (Match #42) has been shuffled! "
            "Betting is open for ~15 more minutes. Use `/bet` now!"
        ]

    @pytest.mark.asyncio
    async def test_notify_betting_awaits_dms_before_returning(
        self, reminder_service, mock_bot
    ):
        """DM sends are awaited rather than spawned as unreferenced tasks.

        The event loop only holds tasks weakly, so a fire-and-forget
        ``create_task`` with no retained handle could be garbage-collected
        before the DM ran. Awaiting means every DM attempt has completed by
        the time this coroutine returns.
        """
        reminder_service.toggle_preference(1, TEST_GUILD_ID, "betting")
        reminder_service.toggle_preference(2, TEST_GUILD_ID, "betting")
        sent_to = []

        async def fake_dm(bot, discord_id, message):
            await asyncio.sleep(0)
            sent_to.append(discord_id)

        with patch.object(reminder_service, "_dm_user", new=fake_dm):
            await reminder_service.notify_betting_subscribers(
                mock_bot, TEST_GUILD_ID, int(time.time()) + 900
            )

        # No extra event-loop yields: delivery finished with the call itself.
        assert set(sent_to) == {1, 2}


# ---------------------------------------------------------------------------
# Dig reminder
# ---------------------------------------------------------------------------


@pytest.fixture
def dig_service_mock():
    mock = MagicMock()
    mock.get_free_dig_ready_at.return_value = None

    def get_free_dig_ready_times_bulk(discord_ids, guild_id, *, now):
        return {
            discord_id: mock.get_free_dig_ready_at(
                discord_id,
                guild_id,
                now=now,
            )
            for discord_id in discord_ids
        }

    mock.get_free_dig_ready_times_bulk.side_effect = get_free_dig_ready_times_bulk
    return mock


@pytest.fixture
def reminder_service_with_dig(notification_repo, player_repo_mock, dig_service_mock):
    return ReminderService(
        notification_repo=notification_repo,
        player_repo=player_repo_mock,
        dig_service=dig_service_mock,
    )


class TestDigReminder:
    def test_dig_preference_toggle(self, notification_repo):
        notification_repo.set_preference(1, TEST_GUILD_ID, "dig", True)
        assert notification_repo.get_preferences(1, TEST_GUILD_ID)["dig_enabled"] is True

    @pytest.mark.asyncio
    async def test_dig_schedule_creates_task_when_enabled(
        self, reminder_service_with_dig, mock_bot
    ):
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        future_time = int(time.time()) + 3600
        key = (1, TEST_GUILD_ID, "dig")

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new_callable=AsyncMock,
        ) as send_dm_after_delay:
            reminder_service_with_dig.schedule_dig_reminder(
                mock_bot, 1, TEST_GUILD_ID, future_time,
            )
            assert key in reminder_service_with_dig._tasks
            task = reminder_service_with_dig._tasks[key]
            try:
                assert send_dm_after_delay.call_args.kwargs["message"] == (
                    "Your free dig cooldown has expired! "
                    "You can `/dig go` again now."
                )
            finally:
                reminder_service_with_dig._cancel_task(1, TEST_GUILD_ID, "dig")
                with pytest.raises(asyncio.CancelledError):
                    await task

    def test_dig_no_task_when_disabled(self, reminder_service_with_dig, mock_bot):
        future_time = int(time.time()) + 3600
        reminder_service_with_dig.schedule_dig_reminder(mock_bot, 1, TEST_GUILD_ID, future_time)
        assert (1, TEST_GUILD_ID, "dig") not in reminder_service_with_dig._tasks

    @pytest.mark.asyncio
    async def test_reschedule_dig_uses_authoritative_ready_at(
        self,
        reminder_service_with_dig,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        ready_at = now + 5400
        monkeypatch.setattr(time, "time", lambda: now)
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_service_mock.get_free_dig_ready_at.return_value = ready_at

        with patch.object(
            reminder_service_with_dig, "schedule_dig_reminder",
        ) as schedule_dig_reminder:
            await reminder_service_with_dig.reschedule_all(mock_bot, [TEST_GUILD_ID])

        dig_service_mock.get_free_dig_ready_at.assert_called_once_with(
            1, TEST_GUILD_ID, now=now,
        )
        schedule_dig_reminder.assert_called_once_with(
            mock_bot,
            1,
            TEST_GUILD_ID,
            ready_at,
            preference_enabled=True,
        )

    @pytest.mark.asyncio
    async def test_reschedule_dig_skips_already_ready(
        self,
        reminder_service_with_dig,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_service_mock.get_free_dig_ready_at.return_value = None

        with patch.object(
            reminder_service_with_dig, "schedule_dig_reminder",
        ) as schedule_dig_reminder:
            await reminder_service_with_dig.reschedule_all(mock_bot, [TEST_GUILD_ID])

        dig_service_mock.get_free_dig_ready_at.assert_called_once_with(
            1, TEST_GUILD_ID, now=now,
        )
        schedule_dig_reminder.assert_not_called()
        assert (1, TEST_GUILD_ID, "dig") not in reminder_service_with_dig._tasks

    @pytest.mark.asyncio
    async def test_reconcile_schedules_exact_authoritative_timestamp(
        self,
        reminder_service_with_dig,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        ready_at = now + 9_000
        key = (1, TEST_GUILD_ID, "dig")
        calls = []
        send_started = asyncio.Event()
        release_send = asyncio.Event()

        async def fake_send(**kwargs):
            calls.append(kwargs)
            send_started.set()
            await release_send.wait()

        monkeypatch.setattr(time, "time", lambda: now)
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_service_mock.get_free_dig_ready_at.return_value = ready_at

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new=fake_send,
        ):
            await reminder_service_with_dig.reconcile_dig_reminder(
                mock_bot,
                1,
                TEST_GUILD_ID,
                now=now,
            )
            await send_started.wait()
            task = reminder_service_with_dig._tasks[key]
            try:
                dig_service_mock.get_free_dig_ready_at.assert_called_once_with(
                    1,
                    TEST_GUILD_ID,
                    now=now,
                )
                assert len(calls) == 1
                assert calls[0]["delay"] == ready_at - now
                assert calls[0]["message"] == (
                    "Your free dig cooldown has expired! "
                    "You can `/dig go` again now."
                )
            finally:
                reminder_service_with_dig.cancel_dig_reminder(1, TEST_GUILD_ID)
                await asyncio.gather(task, return_exceptions=True)

    @pytest.mark.asyncio
    @pytest.mark.parametrize("ready_offset", [None, 0, -1])
    async def test_reconcile_cancels_stale_task_when_already_ready(
        self,
        ready_offset,
        reminder_service_with_dig,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        key = (1, TEST_GUILD_ID, "dig")
        send_started = asyncio.Event()
        release_send = asyncio.Event()

        async def fake_send(**_kwargs):
            send_started.set()
            await release_send.wait()

        monkeypatch.setattr(time, "time", lambda: now)
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new=fake_send,
        ):
            reminder_service_with_dig.schedule_dig_reminder(
                mock_bot,
                1,
                TEST_GUILD_ID,
                now + 100,
            )
            stale_task = reminder_service_with_dig._tasks[key]
            await send_started.wait()
            dig_service_mock.get_free_dig_ready_at.return_value = (
                None if ready_offset is None else now + ready_offset
            )

            await reminder_service_with_dig.reconcile_dig_reminder(
                mock_bot,
                1,
                TEST_GUILD_ID,
                now=now,
            )
            await asyncio.gather(stale_task, return_exceptions=True)

        assert key not in reminder_service_with_dig._tasks
        assert stale_task.cancelled()

    @pytest.mark.asyncio
    async def test_repeated_reconcile_replaces_with_one_task(
        self,
        reminder_service_with_dig,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        key = (1, TEST_GUILD_ID, "dig")
        calls = []
        second_send_started = asyncio.Event()
        release_send = asyncio.Event()

        async def fake_send(**kwargs):
            calls.append(kwargs)
            if len(calls) == 2:
                second_send_started.set()
            await release_send.wait()

        monkeypatch.setattr(time, "time", lambda: now)
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_service_mock.get_free_dig_ready_at.side_effect = [now + 100, now + 200]

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new=fake_send,
        ):
            await reminder_service_with_dig.reconcile_dig_reminder(
                mock_bot,
                1,
                TEST_GUILD_ID,
                now=now,
            )
            first_task = reminder_service_with_dig._tasks[key]
            await asyncio.sleep(0)
            await reminder_service_with_dig.reconcile_dig_reminder(
                mock_bot,
                1,
                TEST_GUILD_ID,
                now=now,
            )
            second_task = reminder_service_with_dig._tasks[key]
            await second_send_started.wait()
            try:
                assert first_task is not second_task
                assert first_task.cancelled()
                assert list(reminder_service_with_dig._tasks) == [key]
                assert [call["delay"] for call in calls] == [100, 200]
            finally:
                reminder_service_with_dig.cancel_dig_reminder(1, TEST_GUILD_ID)
                await asyncio.gather(first_task, second_task, return_exceptions=True)

    @pytest.mark.asyncio
    async def test_reconcile_keeps_same_user_guilds_independent(
        self,
        reminder_service_with_dig,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        guild_ready_at = {
            TEST_GUILD_ID: now + 100,
            TEST_GUILD_ID_2: now + 200,
        }
        calls = []
        both_sends_started = asyncio.Event()
        release_send = asyncio.Event()

        def get_ready_at(_discord_id, guild_id, *, now):
            return guild_ready_at[guild_id]

        async def fake_send(**kwargs):
            calls.append(kwargs)
            if len(calls) == 2:
                both_sends_started.set()
            await release_send.wait()

        monkeypatch.setattr(time, "time", lambda: now)
        for guild_id in guild_ready_at:
            reminder_service_with_dig.toggle_preference(1, guild_id, "dig")
        dig_service_mock.get_free_dig_ready_at.side_effect = get_ready_at

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new=fake_send,
        ):
            for guild_id in guild_ready_at:
                await reminder_service_with_dig.reconcile_dig_reminder(
                    mock_bot,
                    1,
                    guild_id,
                    now=now,
                )
            await both_sends_started.wait()
            guild_one_key = (1, TEST_GUILD_ID, "dig")
            guild_two_key = (1, TEST_GUILD_ID_2, "dig")
            guild_one_task = reminder_service_with_dig._tasks[guild_one_key]
            guild_two_task = reminder_service_with_dig._tasks[guild_two_key]
            try:
                assert set(reminder_service_with_dig._tasks) == {
                    guild_one_key,
                    guild_two_key,
                }
                assert sorted(call["delay"] for call in calls) == [100, 200]
                reminder_service_with_dig.cancel_dig_reminder(1, TEST_GUILD_ID)
                assert guild_one_key not in reminder_service_with_dig._tasks
                assert reminder_service_with_dig._tasks[guild_two_key] is guild_two_task
            finally:
                reminder_service_with_dig.cancel_dig_reminder(1, TEST_GUILD_ID)
                reminder_service_with_dig.cancel_dig_reminder(1, TEST_GUILD_ID_2)
                await asyncio.gather(
                    guild_one_task,
                    guild_two_task,
                    return_exceptions=True,
                )

    @pytest.mark.asyncio
    async def test_stale_r0_reconcile_cannot_overwrite_live_r1(
        self,
        reminder_service_with_dig,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        r0 = 1_000_000
        r1 = r0 + 100
        old_ready_at = r0 + 7_200
        # Simulates a boss loss whose persisted anchor includes a cooldown stinger.
        loss_with_stinger_ready_at = r1 + 9_000
        key = (1, TEST_GUILD_ID, "dig")
        first_read_started = threading.Event()
        release_first_read = threading.Event()
        authoritative_reads = []
        sends = []
        send_started = asyncio.Event()
        release_send = asyncio.Event()

        def get_ready_at(_discord_id, _guild_id, *, now):
            authoritative_reads.append(now)
            if len(authoritative_reads) == 1:
                first_read_started.set()
                release_first_read.wait()
                return old_ready_at
            return loss_with_stinger_ready_at

        async def fake_send(**kwargs):
            sends.append(kwargs)
            send_started.set()
            await release_send.wait()

        monkeypatch.setattr(time, "time", lambda: r1)
        reminder_service_with_dig.toggle_preference(1, TEST_GUILD_ID, "dig")
        dig_service_mock.get_free_dig_ready_at.side_effect = get_ready_at

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new=fake_send,
        ):
            stale_reconcile = asyncio.create_task(
                reminder_service_with_dig.reconcile_dig_reminder(
                    mock_bot,
                    1,
                    TEST_GUILD_ID,
                    now=r0,
                )
            )
            await asyncio.to_thread(first_read_started.wait)
            live_reconcile = asyncio.create_task(
                reminder_service_with_dig.reconcile_dig_reminder(
                    mock_bot,
                    1,
                    TEST_GUILD_ID,
                    now=r1,
                )
            )
            await asyncio.sleep(0)
            assert reminder_service_with_dig._dig_reconcile_versions[key] == 2
            release_first_read.set()
            await asyncio.gather(stale_reconcile, live_reconcile)
            await send_started.wait()
            reminder_task = reminder_service_with_dig._tasks[key]
            try:
                assert authoritative_reads == [r0, r1]
                assert len(sends) == 1
                assert sends[0]["delay"] == loss_with_stinger_ready_at - r1
                assert sends[0]["delay"] != old_ready_at - r1
                assert list(reminder_service_with_dig._tasks) == [key]
            finally:
                reminder_service_with_dig.cancel_dig_reminder(1, TEST_GUILD_ID)
                await asyncio.gather(reminder_task, return_exceptions=True)

    @pytest.mark.asyncio
    async def test_recovery_completely_reconciles_existing_dig_tasks(
        self,
        reminder_service_with_dig,
        notification_repo,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        release_send = asyncio.Event()

        async def fake_send(**_kwargs):
            await release_send.wait()

        monkeypatch.setattr(time, "time", lambda: now)
        for discord_id in (1, 2):
            notification_repo.set_preference(discord_id, TEST_GUILD_ID, "dig", True)

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new=fake_send,
        ):
            for discord_id in (1, 2):
                reminder_service_with_dig.schedule_dig_reminder(
                    mock_bot,
                    discord_id,
                    TEST_GUILD_ID,
                    now + 100,
                )
            old_tasks = list(reminder_service_with_dig._tasks.values())
            notification_repo.set_preference(1, TEST_GUILD_ID, "dig", False)
            dig_service_mock.get_free_dig_ready_at.return_value = None

            await reminder_service_with_dig.reschedule_all(mock_bot, [TEST_GUILD_ID])
            await asyncio.gather(*old_tasks, return_exceptions=True)

        assert not [
            key
            for key in reminder_service_with_dig._tasks
            if key[1] == TEST_GUILD_ID and key[2] == "dig"
        ]

    @pytest.mark.asyncio
    async def test_recovery_continues_when_bulk_dig_result_omits_subscriber(
        self,
        reminder_service_with_dig,
        notification_repo,
        dig_service_mock,
        mock_bot,
        monkeypatch,
    ):
        now = 1_000_000
        observed = []
        release_send = asyncio.Event()

        def get_ready_times(discord_ids, guild_id, *, now):
            ready_times = {}
            for discord_id in discord_ids:
                observed.append((discord_id, guild_id, now))
                if (discord_id, guild_id) != (1, TEST_GUILD_ID):
                    ready_times[discord_id] = now + 100
            return ready_times

        async def fake_send(**_kwargs):
            await release_send.wait()

        monkeypatch.setattr(time, "time", lambda: now)
        for discord_id, guild_id in (
            (1, TEST_GUILD_ID),
            (2, TEST_GUILD_ID),
            (1, TEST_GUILD_ID_2),
        ):
            notification_repo.set_preference(discord_id, guild_id, "dig", True)
        dig_service_mock.get_free_dig_ready_times_bulk.side_effect = get_ready_times

        with patch.object(
            reminder_service_with_dig,
            "_send_dm_after_delay",
            new=fake_send,
        ):
            await reminder_service_with_dig.reschedule_all(
                mock_bot,
                [TEST_GUILD_ID, TEST_GUILD_ID_2],
            )
            tasks = list(reminder_service_with_dig._tasks.values())
            try:
                assert set(observed) == {
                    (1, TEST_GUILD_ID, now),
                    (2, TEST_GUILD_ID, now),
                    (1, TEST_GUILD_ID_2, now),
                }
                assert (2, TEST_GUILD_ID, "dig") in reminder_service_with_dig._tasks
                assert (1, TEST_GUILD_ID_2, "dig") in reminder_service_with_dig._tasks
            finally:
                for discord_id, guild_id, _ in observed:
                    reminder_service_with_dig.cancel_dig_reminder(discord_id, guild_id)
                await asyncio.gather(*tasks, return_exceptions=True)

    @pytest.mark.asyncio
    async def test_recovery_continues_after_guild_subscriber_snapshot_failure(
        self,
        reminder_service,
        notification_repo,
        player_repo_mock,
        mock_bot,
        monkeypatch,
        caplog,
    ):
        now = 1_000_000
        original_get_subscribers = notification_repo.get_enabled_users_by_type_bulk

        def get_subscribers(guild_id, reminder_types):
            if guild_id == TEST_GUILD_ID:
                raise RuntimeError("broken guild snapshot")
            return original_get_subscribers(guild_id, reminder_types)

        monkeypatch.setattr(time, "time", lambda: now)
        caplog.set_level(logging.ERROR, logger="cama_bot.reminder_service")
        notification_repo.set_preference(1, TEST_GUILD_ID, "trivia", True)
        notification_repo.set_preference(2, TEST_GUILD_ID_2, "wheel", True)
        player_repo_mock.get_last_trivia_session.return_value = now
        player_repo_mock.get_last_wheel_spin.return_value = now

        with patch.object(
            notification_repo,
            "get_enabled_users_by_type_bulk",
            side_effect=get_subscribers,
        ):
            await reminder_service.reschedule_all(
                mock_bot,
                [TEST_GUILD_ID, TEST_GUILD_ID_2],
            )

        tasks = list(reminder_service._tasks.values())
        try:
            assert (1, TEST_GUILD_ID, "trivia") not in reminder_service._tasks
            assert (2, TEST_GUILD_ID_2, "wheel") in reminder_service._tasks
            assert (
                f"Failed to load reminder subscribers for guild_id={TEST_GUILD_ID}"
            ) in caplog.text
        finally:
            reminder_service._cancel_task(2, TEST_GUILD_ID_2, "wheel")
            await asyncio.gather(*tasks, return_exceptions=True)


class TestArmPreferenceOnEnable:
    """Enabling a reminder mid-cooldown must arm the reminder already owed."""

    @pytest.mark.asyncio
    async def test_arm_preference_schedules_pending_wheel_cooldown(
        self, reminder_service, player_repo_mock, mock_bot
    ):
        """Turning Gamba on 3h into a 24h cooldown must schedule that DM.

        Every schedule_* call site fires on an action (a spin), so enabling the
        preference used to arm nothing — the user got no DM for the cooldown
        they were already in.
        """
        from config import WHEEL_COOLDOWN_SECONDS

        discord_id = 4242
        spun_at = int(time.time()) - 3 * 3600
        player_repo_mock.get_last_wheel_spin.return_value = spun_at

        reminder_service.set_preference(discord_id, TEST_GUILD_ID, "wheel", True)
        assert not reminder_service._tasks, "no task should exist before arming"

        await reminder_service.arm_preference(mock_bot, discord_id, TEST_GUILD_ID, "wheel")

        key = (discord_id, TEST_GUILD_ID, "wheel")
        assert key in reminder_service._tasks, "enabling must arm the pending cooldown"

        reminder_service._tasks[key].cancel()
        assert WHEEL_COOLDOWN_SECONDS > 0

    @pytest.mark.asyncio
    async def test_arm_preference_is_a_noop_with_no_pending_cooldown(
        self, reminder_service, player_repo_mock, mock_bot
    ):
        """A user who has never spun has nothing owed, so nothing is scheduled."""
        discord_id = 4243
        player_repo_mock.get_last_wheel_spin.return_value = None

        reminder_service.set_preference(discord_id, TEST_GUILD_ID, "wheel", True)
        await reminder_service.arm_preference(mock_bot, discord_id, TEST_GUILD_ID, "wheel")

        assert (discord_id, TEST_GUILD_ID, "wheel") not in reminder_service._tasks

    @pytest.mark.asyncio
    async def test_arm_preference_ignores_elapsed_cooldown(
        self, reminder_service, player_repo_mock, mock_bot
    ):
        """A cooldown that already expired needs no reminder."""
        from config import WHEEL_COOLDOWN_SECONDS

        discord_id = 4244
        player_repo_mock.get_last_wheel_spin.return_value = (
            int(time.time()) - WHEEL_COOLDOWN_SECONDS - 60
        )

        reminder_service.set_preference(discord_id, TEST_GUILD_ID, "wheel", True)
        await reminder_service.arm_preference(mock_bot, discord_id, TEST_GUILD_ID, "wheel")

        assert (discord_id, TEST_GUILD_ID, "wheel") not in reminder_service._tasks

    @pytest.mark.asyncio
    async def test_arm_preference_ignores_event_driven_types(
        self, reminder_service, mock_bot
    ):
        """lobby/betting reminders are event-driven; nothing is owed on enable."""
        for rtype in ("lobby", "betting"):
            await reminder_service.arm_preference(mock_bot, 4245, TEST_GUILD_ID, rtype)
        assert not reminder_service._tasks


# ---------------------------------------------------------------------------
# Lobby target subscription concurrency
# ---------------------------------------------------------------------------


class TestLobbyTargetSubscriptionConcurrency:
    def test_two_concurrent_claimers_have_exactly_one_winner(
        self,
        notification_repo,
    ):
        """BEGIN IMMEDIATE makes a one-shot subscription consumable once."""
        subscriber_id = 7001
        target_id = 7002
        assert notification_repo.add_lobby_target_subscription(
            subscriber_id,
            target_id,
            TEST_GUILD_ID,
        )

        repositories = [
            NotificationRepository(notification_repo.db_path),
            NotificationRepository(notification_repo.db_path),
        ]
        start = threading.Barrier(2)
        results: list[list[int] | None] = [None, None]
        errors: list[BaseException] = []
        errors_lock = threading.Lock()

        def claim(index: int) -> None:
            try:
                start.wait(timeout=2)
                results[index] = repositories[index].claim_lobby_target_subscribers(
                    target_id,
                    TEST_GUILD_ID,
                )
            except BaseException as exc:  # surfaced on the test thread below
                with errors_lock:
                    errors.append(exc)

        claimers = [threading.Thread(target=claim, args=(index,)) for index in range(2)]
        for claimer in claimers:
            claimer.start()
        for claimer in claimers:
            claimer.join(timeout=5)

        assert not any(claimer.is_alive() for claimer in claimers)
        assert errors == []
        assert sorted(results, key=lambda claimed: len(claimed or [])) == [
            [],
            [subscriber_id],
        ]
