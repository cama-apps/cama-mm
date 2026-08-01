import io
import logging
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import discord
import pytest

import commands.dig as dig_module
import commands.dig_helpers.boss_views as boss_views
from commands.dig import DigCommands
from utils import dig_assets


@pytest.mark.asyncio
async def test_schedule_dig_reminder_delegates_to_reconciliation():
    reconcile_dig_reminder = AsyncMock()
    bot = SimpleNamespace(
        reminder_service=SimpleNamespace(
            reconcile_dig_reminder=reconcile_dig_reminder,
        )
    )
    cog = DigCommands(bot, SimpleNamespace())

    await cog._schedule_dig_reminder(10001, 12345)

    reconcile_dig_reminder.assert_awaited_once_with(bot, 10001, 12345)


@pytest.mark.asyncio
async def test_schedule_dig_reminder_reuses_ready_timestamp():
    reconcile_dig_reminder = AsyncMock()
    bot = SimpleNamespace(
        reminder_service=SimpleNamespace(
            reconcile_dig_reminder=reconcile_dig_reminder,
        )
    )
    cog = DigCommands(bot, SimpleNamespace())

    await cog._schedule_dig_reminder(10001, 12345, 1_000_500)

    reconcile_dig_reminder.assert_awaited_once_with(
        bot,
        10001,
        12345,
        ready_at=1_000_500,
    )


@pytest.mark.asyncio
async def test_run_dig_reuses_registration_check_and_embedded_notice():
    dig = Mock(
        return_value={
            "success": True,
            "relic_trim_notice": False,
            "next_free_dig_at": 1_000_500,
        }
    )
    dig_service = SimpleNamespace(
        dig=dig,
        pop_relic_trim_notice=Mock(),
    )
    cog = DigCommands(SimpleNamespace(), dig_service)

    result = await cog._run_dig(
        10001,
        12345,
        paid=True,
        player_verified=True,
    )

    assert result.next_free_dig_at == 1_000_500
    dig.assert_called_once_with(
        10001,
        12345,
        paid=True,
        player_verified=True,
    )
    dig_service.pop_relic_trim_notice.assert_not_called()


@pytest.mark.asyncio
async def test_schedule_dig_reminder_is_noop_without_service():
    cog = DigCommands(SimpleNamespace(), SimpleNamespace())

    await cog._schedule_dig_reminder(10001, 12345)


@pytest.mark.asyncio
async def test_reconciliation_failure_does_not_interrupt_dig_and_warns(caplog):
    reconcile_dig_reminder = AsyncMock(
        side_effect=RuntimeError("reconciliation failed"),
    )
    bot = SimpleNamespace(
        reminder_service=SimpleNamespace(
            reconcile_dig_reminder=reconcile_dig_reminder,
        )
    )
    cog = DigCommands(bot, SimpleNamespace())

    with caplog.at_level(logging.WARNING, logger="cama_bot.commands.dig"):
        await cog._schedule_dig_reminder(10001, 12345)

    reconcile_dig_reminder.assert_awaited_once_with(bot, 10001, 12345)
    assert any(
        record.levelno == logging.WARNING
        and record.getMessage()
        == "dig reminder scheduling failed for user 10001 in guild 12345"
        for record in caplog.records
    )


@pytest.mark.asyncio
async def test_boss_encounter_receives_reminder_callback(monkeypatch):
    captured = {}

    class FakeBossEncounterView:
        def __init__(self, *args, **kwargs):
            captured.update(kwargs)

    monkeypatch.setattr(dig_module, "BossEncounterView", FakeBossEncounterView)
    bot = SimpleNamespace()
    dig_service = SimpleNamespace(
        has_scout_lantern=lambda user_id, guild_id: False,
    )
    cog = DigCommands(bot, dig_service)
    cog._send_public_dig = AsyncMock(return_value=None)
    interaction = SimpleNamespace(user=SimpleNamespace(id=10001))
    result = SimpleNamespace(
        boss_info=SimpleNamespace(
            name="Test Boss",
            dialogue="Test dialogue",
            boundary=None,
            luminosity_display=None,
        )
    )

    await cog._handle_boss_encounter(interaction, 12345, result)

    callback = captured["on_boss_resolved"]
    assert callback.__self__ is cog
    assert callback.__func__ is DigCommands._schedule_dig_reminder


@pytest.mark.asyncio
async def test_boss_encounter_embed_surfaces_carried_wager(monkeypatch):
    monkeypatch.setattr(dig_module, "BossEncounterView", Mock())
    bot = SimpleNamespace()
    dig_service = SimpleNamespace(
        has_scout_lantern=lambda user_id, guild_id: False,
    )
    cog = DigCommands(bot, dig_service)
    cog._send_public_dig = AsyncMock(return_value=None)
    interaction = SimpleNamespace(user=SimpleNamespace(id=10001))
    result = SimpleNamespace(
        boss_info=SimpleNamespace(
            name="Test Boss",
            dialogue="Test dialogue",
            boundary=None,
            luminosity_display=None,
            carried_wager=1_500,
        )
    )

    await cog._handle_boss_encounter(interaction, 12345, result)

    embed = cog._send_public_dig.await_args.kwargs["embed"]
    carried_field = next(
        (field for field in embed.fields if field.name == "Carried Wager"),
        None,
    )
    assert carried_field is not None
    assert "**1,500**" in carried_field.value


@pytest.mark.parametrize(
    (
        "phase",
        "roll",
        "canonical_name",
        "canonical_dialogue",
        "expected_name",
        "expected_dialogue",
        "filename",
    ),
    [
        (
            2,
            0.99,
            "The Crowned Hunger",
            "The crown burns.",
            "The Crowned Hunger",
            "The crown burns.",
            "pinnacle_phase_2.png",
        ),
        (
            3,
            0.0,
            "The Last Breath of Kings",
            "Last breath.",
            "The Crown Remembers",
            "A king's last room opens behind the throne.",
            "pinnacle_phase_3.gif",
        ),
    ],
)
@pytest.mark.asyncio
async def test_resumed_pinnacle_encounter_uses_phase_presentation(
    monkeypatch,
    phase,
    roll,
    canonical_name,
    canonical_dialogue,
    expected_name,
    expected_dialogue,
    filename,
):
    class FixedRandom:
        def random(self):
            return roll

        def choice(self, values):
            return values[-1]

    # Random(seed_key) now pins the roll to one encounter, so the stub has to
    # accept the seed argument.
    monkeypatch.setattr(boss_views.random, "Random", lambda *_a: FixedRandom())
    phase_art = discord.File(io.BytesIO(b"phase"), filename=filename)
    monkeypatch.setattr(
        dig_assets,
        "get_pinnacle_phase_art",
        AsyncMock(return_value=phase_art),
    )
    monkeypatch.setattr(dig_module, "BossEncounterView", Mock())

    dig_service = SimpleNamespace(
        has_scout_lantern=lambda user_id, guild_id: False,
    )
    cog = DigCommands(SimpleNamespace(), dig_service)
    cog._send_public_dig = AsyncMock(return_value=None)
    interaction = SimpleNamespace(user=SimpleNamespace(id=10001))
    result = SimpleNamespace(
        depth=350,
        boss_info=SimpleNamespace(
            boss_id="forgotten_king",
            name=canonical_name,
            dialogue=canonical_dialogue,
            boundary=350,
            phase=phase,
            is_pinnacle=True,
            luminosity_display=None,
            wager_allowed=True,
        ),
    )

    await cog._handle_boss_encounter(interaction, 12345, result)

    sent = cog._send_public_dig.await_args.kwargs
    assert sent["embed"].title == f"Boss Encountered: {expected_name}!"
    assert sent["embed"].description == expected_dialogue
    assert sent["embed"].image.url == f"attachment://{filename}"
    assert sent["file"] is phase_art
