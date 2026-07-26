"""Tests for the /pet cog's background sweep delivery and match hook."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

import commands.pet as pet_commands
from commands.pet import PetCommands, setup
from domain.models.pet import (
    DeathNotice,
    HatchNotice,
    Pet,
    RefundNotice,
    RefundPayout,
)
from domain.pet_constants import EGG_HATCH_SECONDS
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000


def make_pet(**overrides) -> Pet:
    defaults = {
        "pet_id": 1,
        "discord_id": 111,
        "guild_id": TEST_GUILD_ID,
        "name": "Blep",
        "species": "common_cama",
        "adopted_at": T0,
        "hatched_at": T0 + EGG_HATCH_SECONDS,
        "adopt_fee": 20,
        "last_fed_at": T0 + EGG_HATCH_SECONDS,
        "hunger_at_last_fed": 100,
        "times_fed": 0,
        "feeds_today": 0,
        "feed_date": None,
        "week_consumed_jc": 0,
        "week_key": None,
        "prev_week_consumed_jc": 0,
        "prev_week_key": None,
        "pampered_until": None,
        "aegis_used": 0,
        "hatch_announced_at": None,
        "died_at": None,
        "death_cause": None,
        "death_announced_at": None,
    }
    defaults.update(overrides)
    return Pet(**defaults)


def make_cog(sweep_result=None, channel=None) -> PetCommands:
    service = MagicMock()
    service.sweep.return_value = sweep_result or {
        "hatches": [], "deaths": [], "refunds": []
    }
    bot = MagicMock()
    cog = PetCommands.__new__(PetCommands)
    cog.bot = bot
    cog.pet_service = service
    if channel is not None:
        with patch.object(pet_commands, "PET_CHANNEL_ID", 42):
            pass
    return cog


def forbidden() -> discord.Forbidden:
    response = MagicMock()
    response.status = 403
    return discord.Forbidden(response, "no access")


@pytest.fixture
def channel():
    ch = MagicMock(spec=discord.TextChannel)
    ch.send = AsyncMock()
    return ch


class TestSweepDelivery:
    @pytest.mark.asyncio
    async def test_hatch_posts_to_channel_and_marks(self, channel, monkeypatch):
        pet = make_pet()
        cog = make_cog({"hatches": [HatchNotice(pet=pet)], "deaths": [], "refunds": []})
        monkeypatch.setattr(cog, "_pet_channel", lambda gid: channel)
        await cog._pet_sweep_loop.coro(cog)
        channel.send.assert_awaited_once()
        cog.pet_service.mark_hatch_announced.assert_called_once_with(pet)

    @pytest.mark.asyncio
    async def test_no_channel_still_marks_announced(self, monkeypatch):
        pet = make_pet(died_at=T0 + 9 * 86400, death_cause="starvation")
        cog = make_cog({"hatches": [], "deaths": [DeathNotice(pet=pet)], "refunds": []})
        monkeypatch.setattr(cog, "_pet_channel", lambda gid: None)
        await cog._pet_sweep_loop.coro(cog)
        cog.pet_service.mark_death_announced.assert_called_once_with(pet)

    @pytest.mark.asyncio
    async def test_forbidden_marks_announced(self, channel, monkeypatch):
        pet = make_pet(died_at=T0 + 9 * 86400, death_cause="starvation")
        channel.send.side_effect = forbidden()
        cog = make_cog({"hatches": [], "deaths": [DeathNotice(pet=pet)], "refunds": []})
        monkeypatch.setattr(cog, "_pet_channel", lambda gid: channel)
        await cog._pet_sweep_loop.coro(cog)
        cog.pet_service.mark_death_announced.assert_called_once_with(pet)

    @pytest.mark.asyncio
    async def test_transient_error_leaves_unmarked_for_retry(
        self, channel, monkeypatch
    ):
        pet = make_pet(died_at=T0 + 9 * 86400, death_cause="starvation")
        channel.send.side_effect = RuntimeError("discord hiccup")
        cog = make_cog({"hatches": [], "deaths": [DeathNotice(pet=pet)], "refunds": []})
        monkeypatch.setattr(cog, "_pet_channel", lambda gid: channel)
        await cog._pet_sweep_loop.coro(cog)
        cog.pet_service.mark_death_announced.assert_not_called()

    @pytest.mark.asyncio
    async def test_one_bad_notice_does_not_block_others(self, channel, monkeypatch):
        bad = make_pet(pet_id=1, died_at=T0 + 9 * 86400, death_cause="starvation")
        good = make_pet(pet_id=2, discord_id=222, died_at=T0 + 9 * 86400,
                        death_cause="starvation")
        calls = {"n": 0}

        async def flaky_send(*args, **kwargs):
            calls["n"] += 1
            if calls["n"] == 1:
                raise RuntimeError("hiccup")

        channel.send = AsyncMock(side_effect=flaky_send)
        cog = make_cog({
            "hatches": [],
            "deaths": [DeathNotice(pet=bad), DeathNotice(pet=good)],
            "refunds": [],
        })
        monkeypatch.setattr(cog, "_pet_channel", lambda gid: channel)
        await cog._pet_sweep_loop.coro(cog)
        cog.pet_service.mark_death_announced.assert_called_once_with(good)

    @pytest.mark.asyncio
    async def test_refund_summary_posts(self, channel, monkeypatch):
        notice = RefundNotice(
            guild_id=TEST_GUILD_ID,
            week_key="2026-W30",
            payouts=(
                RefundPayout(discord_id=111, consumed_jc=20,
                             multiplier_pct=150, amount=30),
            ),
            total_paid=30,
            scaled_down=False,
        )
        cog = make_cog({"hatches": [], "deaths": [], "refunds": [notice]})
        monkeypatch.setattr(cog, "_pet_channel", lambda gid: channel)
        await cog._pet_sweep_loop.coro(cog)
        channel.send.assert_awaited_once()
        embed = channel.send.await_args.kwargs["embed"]
        assert "150%" in embed.description

    @pytest.mark.asyncio
    async def test_sweep_service_failure_is_contained(self):
        cog = make_cog()
        cog.pet_service.sweep.side_effect = RuntimeError("db locked")
        await cog._pet_sweep_loop.coro(cog)  # must not raise


class TestSetup:
    @pytest.mark.asyncio
    async def test_setup_skips_when_feature_disabled(self):
        """No PET_CHANNEL_ID -> no pet_service -> the cog must not load."""
        bot = MagicMock()
        bot.pet_service = None
        await setup(bot)
        bot.add_cog.assert_not_called()

    @pytest.mark.asyncio
    async def test_setup_loads_cog_when_service_present(self):
        bot = MagicMock()
        bot.pet_service = MagicMock()
        bot.add_cog = AsyncMock()
        await setup(bot)
        bot.add_cog.assert_awaited_once()

    def test_container_gates_service_on_channel_id(self, repo_db_path, monkeypatch):
        import config
        from infrastructure.service_container import ServiceContainer

        monkeypatch.setattr(config, "PET_CHANNEL_ID", None)
        container = ServiceContainer(repo_db_path)
        container._init_repositories()
        container._components["player_repo"] = MagicMock()
        container._init_pet_service()
        assert container._components["pet_service"] is None

        monkeypatch.setattr(config, "PET_CHANNEL_ID", 123456)
        container._init_pet_service()
        assert container._components["pet_service"] is not None


class TestPetReminders:
    def test_notification_repo_supports_pet_type(self, repo_db_path):
        from repositories.notification_repository import NotificationRepository

        repo = NotificationRepository(repo_db_path)
        assert repo.get_preferences(111, TEST_GUILD_ID)["pet_enabled"] is False
        repo.set_preference(111, TEST_GUILD_ID, "pet", True)
        assert repo.get_preferences(111, TEST_GUILD_ID)["pet_enabled"] is True
        assert repo.get_enabled_users_for_type(TEST_GUILD_ID, "pet") == [111]

    @pytest.mark.asyncio
    async def test_schedule_pet_reminder_is_pref_gated(self):
        from services.reminder_service import ReminderService

        notification_repo = MagicMock()
        notification_repo.get_preferences.return_value = {"pet_enabled": False}
        service = ReminderService(
            notification_repo=notification_repo,
            player_repo=MagicMock(),
            dig_service=MagicMock(),
        )
        service.schedule_pet_reminder(
            MagicMock(), 111, TEST_GUILD_ID, T0 + 1000, pet_name="Blep"
        )
        assert (111, TEST_GUILD_ID, "pet") not in service._tasks
        notification_repo.get_preferences.return_value = {"pet_enabled": True}
        service.schedule_pet_reminder(
            MagicMock(), 111, TEST_GUILD_ID, T0 + 1000, pet_name="Blep"
        )
        key = (111, TEST_GUILD_ID, "pet")
        assert key in service._tasks
        service._tasks[key].cancel()

    @pytest.mark.asyncio
    async def test_past_crossing_schedules_no_task(self):
        from services.reminder_service import ReminderService

        notification_repo = MagicMock()
        notification_repo.get_preferences.return_value = {"pet_enabled": True}
        service = ReminderService(
            notification_repo=notification_repo,
            player_repo=MagicMock(),
            dig_service=MagicMock(),
        )
        # Pet already at/below the warning band: crossing is in the past.
        service.schedule_pet_reminder(
            MagicMock(), 111, TEST_GUILD_ID, 12345, pet_name="Blep"
        )
        assert (111, TEST_GUILD_ID, "pet") not in service._tasks

    @pytest.mark.asyncio
    async def test_rearm_warning_schedules_at_crossing(self):
        cog = make_cog()
        cog.pet_service.decay_per_day = 20
        reminder_svc = MagicMock()
        reminder_svc.get_preferences.return_value = {"pet_enabled": True}
        cog.bot.reminder_service = reminder_svc
        pet = make_pet()
        await cog._rearm_warning(pet)
        args, kwargs = reminder_svc.schedule_pet_reminder.call_args
        # 100 -> 30 is 70 points at 20/day = 3.5 days after the anchor.
        assert args[3] == pet.last_fed_at + 3 * 86400 + 43200
        assert kwargs["pet_name"] == "Blep"
        # The pref was pre-fetched off-loop and passed through.
        assert kwargs["preference_enabled"] is True


class TestPetChannel:
    def test_returns_none_when_unconfigured(self):
        cog = make_cog()
        with patch.object(pet_commands, "PET_CHANNEL_ID", None):
            assert cog._pet_channel(TEST_GUILD_ID) is None

    def test_returns_channel_when_configured(self):
        cog = make_cog()
        text_channel = MagicMock(spec=discord.TextChannel)
        guild = MagicMock()
        guild.get_channel.return_value = text_channel
        cog.bot.get_guild.return_value = guild
        with patch.object(pet_commands, "PET_CHANNEL_ID", 42):
            assert cog._pet_channel(TEST_GUILD_ID) is text_channel
        guild.get_channel.assert_called_once_with(42)
