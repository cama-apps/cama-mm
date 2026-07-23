"""Tests for the Mafia command cog's background behavior."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest

import commands.mafia as mafia_commands
from commands.mafia import MafiaCommands
from domain.models.mafia import MafiaPhase
from repositories.mafia_repository import MafiaRepository
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def mafia_repo(repo_db_path):
    return MafiaRepository(repo_db_path)


@pytest.mark.asyncio
async def test_automatic_reminder_is_deduplicated_across_cog_instances(
    mafia_repo, monkeypatch
):
    game_id = mafia_repo.create_game(
        TEST_GUILD_ID,
        "2026-04-24",
        MafiaPhase.NIGHT,
        started_at=1000,
        roster_size=5,
        twist_event=None,
    )
    game = mafia_repo.get_game_by_id(game_id)
    service = SimpleNamespace(
        repo=mafia_repo,
        players_needing_night_action=MagicMock(return_value=[100]),
    )
    channel = SimpleNamespace(send=AsyncMock())
    guild = SimpleNamespace(id=TEST_GUILD_ID)
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)
    first_cog = MafiaCommands(MagicMock(), service, MagicMock())
    reloaded_cog = MafiaCommands(MagicMock(), service, MagicMock())

    await first_cog._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)
    await reloaded_cog._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)

    channel.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_automatic_reminder_revalidates_recipients_after_claim(
    mafia_repo, monkeypatch
):
    game_id = mafia_repo.create_game(
        TEST_GUILD_ID,
        "2026-04-24",
        MafiaPhase.NIGHT,
        started_at=1000,
        roster_size=5,
        twist_event=None,
    )
    game = mafia_repo.get_game_by_id(game_id)
    service = SimpleNamespace(
        repo=mafia_repo,
        players_needing_night_action=MagicMock(side_effect=[[100], []]),
    )
    channel = SimpleNamespace(send=AsyncMock())
    guild = SimpleNamespace(id=TEST_GUILD_ID)
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)
    cog = MafiaCommands(MagicMock(), service, MagicMock())

    await cog._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)

    channel.send.assert_not_awaited()
    assert service.players_needing_night_action.call_count == 2
    assert (
        mafia_repo.claim_phase_reminder(
            TEST_GUILD_ID, game_id, 1, MafiaPhase.NIGHT
        )
        is True
    )


@pytest.mark.asyncio
async def test_automatic_reminder_releases_claim_when_phase_changes(
    mafia_repo, monkeypatch
):
    game_id = mafia_repo.create_game(
        TEST_GUILD_ID,
        "2026-04-24",
        MafiaPhase.NIGHT,
        started_at=1000,
        roster_size=5,
        twist_event=None,
    )
    game = mafia_repo.get_game_by_id(game_id)
    original_claim = mafia_repo.claim_phase_reminder

    def claim_then_advance(*args):
        claimed = original_claim(*args)
        mafia_repo.set_phase(game_id, MafiaPhase.DAY)
        return claimed

    monkeypatch.setattr(mafia_repo, "claim_phase_reminder", claim_then_advance)
    service = SimpleNamespace(
        repo=mafia_repo,
        players_needing_night_action=MagicMock(return_value=[100]),
    )
    channel = SimpleNamespace(send=AsyncMock())
    guild = SimpleNamespace(id=TEST_GUILD_ID)
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)
    cog = MafiaCommands(MagicMock(), service, MagicMock())

    await cog._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)

    channel.send.assert_not_awaited()
    assert original_claim(TEST_GUILD_ID, game_id, 1, MafiaPhase.NIGHT) is True


@pytest.mark.asyncio
async def test_automatic_reminder_retries_after_http_failure(mafia_repo, monkeypatch):
    game_id = mafia_repo.create_game(
        TEST_GUILD_ID,
        "2026-04-24",
        MafiaPhase.NIGHT,
        started_at=1000,
        roster_size=5,
        twist_event=None,
    )
    game = mafia_repo.get_game_by_id(game_id)
    service = SimpleNamespace(
        repo=mafia_repo,
        players_needing_night_action=MagicMock(return_value=[100]),
    )
    response = MagicMock(status=500, reason="Server Error")
    channel = SimpleNamespace(
        send=AsyncMock(
            side_effect=[discord.HTTPException(response, "failed"), None]
        )
    )
    guild = SimpleNamespace(id=TEST_GUILD_ID)
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)
    first_cog = MafiaCommands(MagicMock(), service, MagicMock())
    reloaded_cog = MafiaCommands(MagicMock(), service, MagicMock())

    await first_cog._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)
    await reloaded_cog._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)

    assert channel.send.await_count == 2


def _interaction(user_id: int = 100):
    return SimpleNamespace(
        guild=SimpleNamespace(id=TEST_GUILD_ID),
        user=SimpleNamespace(id=user_id),
        response=SimpleNamespace(send_message=AsyncMock()),
    )


@pytest.mark.asyncio
async def test_join_command_rejects_unregistered_player(monkeypatch):
    service = SimpleNamespace(
        join=MagicMock(return_value={"ok": False, "error": "not_registered"})
    )
    interaction = _interaction(user_id=999)
    monkeypatch.setattr(
        mafia_commands, "require_mafia_channel", AsyncMock(return_value=True)
    )
    cog = MafiaCommands(MagicMock(), service, MagicMock())

    await cog.join.callback(cog, interaction)

    interaction.response.send_message.assert_awaited_once_with(
        "You need to be registered before joining Mafia.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_join_command_describes_conditional_queue(monkeypatch):
    service = SimpleNamespace(join=MagicMock(return_value={"ok": True}))
    interaction = _interaction()
    monkeypatch.setattr(
        mafia_commands, "require_mafia_channel", AsyncMock(return_value=True)
    )
    cog = MafiaCommands(MagicMock(), service, MagicMock())

    await cog.join.callback(cog, interaction)

    message = interaction.response.send_message.await_args.args[0]
    assert "queued" in message.lower()
    assert "eligible" in message.lower()


@pytest.mark.asyncio
async def test_inactive_status_directs_players_to_join(monkeypatch):
    service = SimpleNamespace(
        get_public_status=MagicMock(return_value={"active": False})
    )
    interaction = _interaction()
    monkeypatch.setattr(
        mafia_commands, "require_mafia_channel", AsyncMock(return_value=True)
    )
    cog = MafiaCommands(MagicMock(), service, MagicMock())

    await cog.status.callback(cog, interaction)

    message = interaction.response.send_message.await_args.args[0]
    assert "/mafia join" in message
    assert "4 AM" not in message


@pytest.mark.asyncio
async def test_info_uses_current_phase_durations(monkeypatch):
    interaction = _interaction()
    monkeypatch.setattr(
        mafia_commands, "require_mafia_channel", AsyncMock(return_value=True)
    )
    cog = MafiaCommands(MagicMock(), MagicMock(), MagicMock())

    await cog.info.callback(cog, interaction)

    embed = interaction.response.send_message.await_args.kwargs["embed"]
    assert "Night (24h)" in embed.description
    assert "Day (24h)" in embed.description
