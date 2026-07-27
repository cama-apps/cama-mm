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
        "accessory": None,
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


def make_interaction(user_id=100, guild_id=TEST_GUILD_ID):
    inter = MagicMock()
    inter.user.id = user_id
    inter.user.display_name = "Owner"
    inter.guild.id = guild_id
    inter.response.defer = AsyncMock()
    inter.response.is_done = MagicMock(return_value=False)
    inter.response.send_message = AsyncMock()
    inter.response.edit_message = AsyncMock()
    inter.followup.send = AsyncMock()
    inter.edit_original_response = AsyncMock()
    inter.original_response = AsyncMock()
    return inter


@pytest.fixture
def live_cog(repo_db_path, monkeypatch):
    """A PetCommands cog over a REAL service/repository stack."""
    from repositories.pet_repository import PetRepository
    from repositories.player_repository import PlayerRepository
    from services.pet_service import PetService

    # The process-global rate limiter would trip across tests sharing a
    # worker (established convention: patch check per commands module).
    monkeypatch.setattr(
        "commands.pet.GLOBAL_RATE_LIMITER.check",
        lambda **kwargs: MagicMock(allowed=True, retry_after_seconds=0),
    )

    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(100, "Owner", TEST_GUILD_ID)
    player_repo.update_balance(100, TEST_GUILD_ID, 1000)
    service = PetService(
        PetRepository(repo_db_path), player_repo, decay_per_day=20
    )
    bot = MagicMock()
    bot.reminder_service = None  # keep the re-arm path quiet
    cog = PetCommands.__new__(PetCommands)
    cog.bot = bot
    cog.pet_service = service
    return cog


async def adopt_via_handler(cog, name="Blep", user_id=100):
    inter = make_interaction(user_id=user_id)
    with patch("services.pet_service.random.choices", return_value=["common_cama"]):
        await cog.adopt.callback(cog, inter, name)
    return inter


class TestCommandHandlers:
    @pytest.mark.asyncio
    async def test_adopt_happy_path_sends_egg_embed(self, live_cog):
        inter = await adopt_via_handler(live_cog)
        kwargs = inter.followup.send.await_args.kwargs
        assert "mysterious egg" in kwargs["embed"].title.lower()
        pet = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert pet is not None and pet.name == "Blep"

    @pytest.mark.asyncio
    async def test_adopt_unregistered_reports_error(self, live_cog):
        inter = make_interaction(user_id=999)
        await live_cog.adopt.callback(live_cog, inter, "Ghost")
        content = inter.followup.send.await_args.kwargs["content"]
        assert content.startswith("❌")

    @pytest.mark.asyncio
    async def test_status_own_pet_returns_view_and_art(self, live_cog):
        await adopt_via_handler(live_cog)
        inter = make_interaction()
        await live_cog.status.callback(live_cog, inter)
        kwargs = inter.followup.send.await_args.kwargs
        assert kwargs["embed"] is not None
        assert kwargs["view"] is not None  # own pet gets the button panel
        assert kwargs["file"] is not None  # procedural egg art attached
        assert kwargs["ephemeral"] is True  # default is private

    @pytest.mark.asyncio
    async def test_status_other_user_has_no_buttons(self, live_cog):
        await adopt_via_handler(live_cog)
        inter = make_interaction(user_id=555)
        member = MagicMock()
        member.id = 100
        member.display_name = "Owner"
        await live_cog.status.callback(live_cog, inter, user=member)
        assert inter.followup.send.await_args.kwargs.get("view") is None

    @pytest.mark.asyncio
    async def test_buy_and_feed_flow_through_handlers(self, live_cog, monkeypatch):
        pet = None
        await adopt_via_handler(live_cog)
        pet = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        inter = make_interaction()
        await live_cog.buy.callback(live_cog, inter, "tango", 3)
        assert "Bought 3× Tango" in inter.followup.send.await_args.kwargs["content"]
        # Hatch + get hungry, then feed via the handler.
        monkeypatch.setattr(
            live_cog.pet_service, "_now", lambda: pet.hatched_at + 86400
        )
        inter2 = make_interaction()
        await live_cog.feed.callback(live_cog, inter2, "tango")
        content = inter2.followup.send.await_args.kwargs["content"]
        assert "munches" in content and "2 left" in content

    @pytest.mark.asyncio
    async def test_feed_egg_rejected_via_handler(self, live_cog):
        await adopt_via_handler(live_cog)
        inter = make_interaction()
        await live_cog.buy.callback(live_cog, inter, "tango", 1)
        inter2 = make_interaction()
        await live_cog.feed.callback(live_cog, inter2, "tango")
        content = inter2.followup.send.await_args.kwargs["content"]
        assert content.startswith("❌")

    @pytest.mark.asyncio
    async def test_trinket_roll_via_handler(self, live_cog, monkeypatch):
        await adopt_via_handler(live_cog)
        pet = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        monkeypatch.setattr(
            live_cog.pet_service, "_now", lambda: pet.hatched_at + 3600
        )
        inter = make_interaction()
        with patch(
            "services.pet_service.random.choices", return_value=["top_hat"]
        ):
            await live_cog.trinket.callback(live_cog, inter)
        content = inter.followup.send.await_args.kwargs["content"]
        assert "Top Hat" in content and "Equipped" in content
        fresh = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert fresh.accessory == "top_hat"

    @pytest.mark.asyncio
    async def test_trinket_wear_branch_via_handler(self, live_cog, monkeypatch):
        await adopt_via_handler(live_cog)
        pet = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        monkeypatch.setattr(
            live_cog.pet_service, "_now", lambda: pet.hatched_at + 3600
        )
        for accessory in ("red_bow", "top_hat"):
            with patch(
                "services.pet_service.random.choices", return_value=[accessory]
            ):
                await live_cog.trinket.callback(live_cog, make_interaction())
        inter = make_interaction()
        await live_cog.trinket.callback(live_cog, inter, wear="red_bow")
        content = inter.followup.send.await_args.kwargs["content"]
        assert "Now wearing" in content and "Red Bow" in content
        fresh = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert fresh.accessory == "red_bow"

    @pytest.mark.asyncio
    async def test_trinket_rejected_for_egg(self, live_cog):
        await adopt_via_handler(live_cog)  # still an egg
        inter = make_interaction()
        await live_cog.trinket.callback(live_cog, inter)
        content = inter.followup.send.await_args.kwargs["content"]
        assert content.startswith("❌")

    @pytest.mark.asyncio
    async def test_trinket_autocomplete_lists_owned_only(self, live_cog, monkeypatch):
        await adopt_via_handler(live_cog)
        pet = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        monkeypatch.setattr(
            live_cog.pet_service, "_now", lambda: pet.hatched_at + 3600
        )
        with patch(
            "services.pet_service.random.choices", return_value=["wool_scarf"]
        ):
            await live_cog.trinket.callback(live_cog, make_interaction())
        inter = make_interaction()
        choices = await live_cog._trinket_autocomplete(inter, "")
        assert [c.value for c in choices] == ["wool_scarf"]
        assert await live_cog._trinket_autocomplete(inter, "top hat") == []

    @pytest.mark.asyncio
    async def test_shop_rename_graveyard_leaderboard_handlers(self, live_cog):
        await adopt_via_handler(live_cog)
        for handler, args in (
            (live_cog.shop, ()),
            (live_cog.rename, ("Sir Blep",)),
            (live_cog.graveyard, ()),
            (live_cog.leaderboard, ()),
        ):
            inter = make_interaction()
            await handler.callback(live_cog, inter, *args)
            assert inter.followup.send.await_count == 1
        assert (
            live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID).name
            == "Sir Blep"
        )


class TestStatusViewButtons:
    def make_view(self, cog, supplies=None, can_feed=True):
        from commands.pet_helpers.views import PetStatusView

        return PetStatusView(
            cog, 100, TEST_GUILD_ID,
            supplies=supplies or {}, species_id="common_cama", can_feed=can_feed,
        )

    @pytest.mark.asyncio
    async def test_owner_check_blocks_other_users(self, live_cog):
        view = self.make_view(live_cog)
        inter = make_interaction(user_id=555)
        assert not await view.interaction_check(inter)
        msg = inter.response.send_message.await_args.args[0]
        assert "not your cama" in msg.lower()
        assert await view.interaction_check(make_interaction(user_id=100))

    @pytest.mark.asyncio
    async def test_feed_button_disabled_without_stock(self, live_cog):
        view = self.make_view(live_cog, supplies={"tango": 0})
        feed_buttons = [b for b in view.children if "Feed" in (b.label or "")]
        assert feed_buttons and all(b.disabled for b in feed_buttons)

    @pytest.mark.asyncio
    async def test_buy_button_purchases_and_refreshes_panel(self, live_cog):
        await adopt_via_handler(live_cog)
        view = self.make_view(live_cog)
        buy_tango = next(
            b for b in view.children if (b.label or "").startswith("Buy Tango")
        )
        inter = make_interaction()
        await buy_tango.callback(inter)
        # Panel refreshed in place via the component response.
        inter.response.edit_message.assert_awaited_once()
        supplies = live_cog.pet_service.pet_repo.get_supplies(100, TEST_GUILD_ID)
        assert supplies == {"tango": 1}

    @pytest.mark.asyncio
    async def test_feed_button_failure_reports_ephemerally(self, live_cog):
        await adopt_via_handler(live_cog)
        # Force stock display but no real stock: feed fails with no_supplies.
        view = self.make_view(live_cog, supplies={"tango": 1})
        feed_tango = next(
            b for b in view.children if (b.label or "").startswith("Feed Tango")
        )
        inter = make_interaction()
        await feed_tango.callback(inter)
        msg = inter.response.send_message.await_args.args[0]
        assert msg.startswith("❌")
        inter.response.edit_message.assert_not_awaited()


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
    async def test_reschedule_all_recovers_pet_warning_after_restart(
        self, live_cog, monkeypatch
    ):
        """Restart recovery: reschedule_all must re-arm pending pet warnings
        from persisted anchors (parity with wheel/trivia/dig hardening)."""
        from services.reminder_service import ReminderService

        await adopt_via_handler(live_cog)
        pet = live_cog.pet_service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        notification_repo = MagicMock()
        notification_repo.get_enabled_users_by_type_bulk.return_value = {
            "wheel": [], "trivia": [], "dig": [], "pet": [100],
        }
        player_repo = MagicMock()
        player_repo.get_reminder_timestamps_bulk.return_value = {}
        service = ReminderService(
            notification_repo=notification_repo,
            player_repo=player_repo,
            dig_service=None,
            pet_service=live_cog.pet_service,
        )
        # No clock patching needed: the pet was just adopted, so its warning
        # crossing (hatch + 3.5 days) is always in the future.
        assert live_cog.pet_service.warning_crossing_for(pet) > pet.hatched_at
        await service.reschedule_all(MagicMock(), [TEST_GUILD_ID])
        key = (100, TEST_GUILD_ID, "pet")
        assert key in service._tasks
        service._tasks[key].cancel()

    @pytest.mark.asyncio
    async def test_reschedule_all_skips_pets_when_service_disabled(self):
        from services.reminder_service import ReminderService

        notification_repo = MagicMock()
        notification_repo.get_enabled_users_by_type_bulk.return_value = {
            "wheel": [], "trivia": [], "dig": [], "pet": [100],
        }
        player_repo = MagicMock()
        player_repo.get_reminder_timestamps_bulk.return_value = {}
        service = ReminderService(
            notification_repo=notification_repo,
            player_repo=player_repo,
            dig_service=None,
            pet_service=None,  # feature gated off
        )
        await service.reschedule_all(MagicMock(), [TEST_GUILD_ID])
        assert (100, TEST_GUILD_ID, "pet") not in service._tasks

    @pytest.mark.asyncio
    async def test_rearm_warning_schedules_at_crossing(self):
        cog = make_cog()
        cog.pet_service.warning_crossing_for = lambda p: p.hunger_crossing_time(30, 20)
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
