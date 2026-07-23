"""Tests for the Mafia command cog's background behavior."""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, call

import discord
import pytest

import commands.mafia as mafia_commands
from commands.mafia import MafiaCommands
from domain.models.mafia import MafiaGame, MafiaPhase, MafiaPlayer, MafiaRole
from repositories.mafia_repository import MafiaRepository
from tests.conftest import TEST_GUILD_ID

GUILD_ID = 100
GAME_ID = 7
STANDINGS_MESSAGE_ID = 300


async def _run_sync(func, *args, **kwargs):
    """Deterministic stand-in for asyncio.to_thread in orchestration tests."""
    return func(*args, **kwargs)


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


@pytest.fixture
def mafia_service():
    repo = SimpleNamespace(
        get_players=MagicMock(return_value=[]),
        set_thread_ids=MagicMock(),
    )
    return SimpleNamespace(
        repo=repo,
        abort_game=MagicMock(),
    )


@pytest.fixture
def flavor_service():
    return SimpleNamespace(
        no_lynch_narration=AsyncMock(return_value="No one was lynched."),
        resolution_narration=AsyncMock(return_value="The town prevails."),
    )


@pytest.fixture
def bot():
    return SimpleNamespace(fetch_channel=AsyncMock())


@pytest.fixture
def cog(bot, mafia_service, flavor_service):
    return MafiaCommands(bot, mafia_service, flavor_service)


@pytest.fixture
def game():
    return MafiaGame(
        game_id=GAME_ID,
        guild_id=GUILD_ID,
        game_date="2026-07-20",
        phase=MafiaPhase.NIGHT,
        started_at=1_700_000_000,
        phase_started_at=1_700_000_000,
        roster_size=5,
        standings_message_id=STANDINGS_MESSAGE_ID,
    )


@pytest.fixture
def board():
    return SimpleNamespace(edit=AsyncMock(), unpin=AsyncMock())


@pytest.fixture
def replacement_board():
    return SimpleNamespace(id=301, pin=AsyncMock())


@pytest.fixture
def channel(board, replacement_board):
    return SimpleNamespace(
        get_partial_message=MagicMock(return_value=board),
        fetch_message=AsyncMock(),
        send=AsyncMock(return_value=replacement_board),
    )


@pytest.fixture
def guild():
    return SimpleNamespace(id=GUILD_ID, get_thread=MagicMock(return_value=None))


@pytest.mark.asyncio
async def test_standings_update_uses_partial_message_without_fetch(
    cog, game, guild, channel, board, monkeypatch
):
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)

    await cog._update_standings_board(guild, game)

    channel.get_partial_message.assert_called_once_with(STANDINGS_MESSAGE_ID)
    channel.fetch_message.assert_not_awaited()
    board.edit.assert_awaited_once()
    channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_standings_update_reposts_when_partial_edit_fails(
    cog,
    game,
    guild,
    channel,
    board,
    replacement_board,
    mafia_service,
    monkeypatch,
):
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)
    board.edit.side_effect = discord.HTTPException(
        SimpleNamespace(status=500, reason="error"),
        "edit failed",
    )

    await cog._update_standings_board(guild, game)

    channel.get_partial_message.assert_called_once_with(STANDINGS_MESSAGE_ID)
    channel.fetch_message.assert_not_awaited()
    channel.send.assert_awaited_once()
    replacement_board.pin.assert_awaited_once()
    mafia_service.repo.set_thread_ids.assert_called_once_with(
        GAME_ID,
        standings_message_id=replacement_board.id,
    )


@pytest.mark.asyncio
async def test_admin_abort_unpins_partial_standings_message(
    cog, guild, channel, board, mafia_service, monkeypatch
):
    monkeypatch.setattr(
        mafia_commands,
        "require_mafia_channel",
        AsyncMock(return_value=True),
    )
    monkeypatch.setattr(mafia_commands, "has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)
    mafia_service.abort_game.return_value = {
        "ok": True,
        "standings_message_id": STANDINGS_MESSAGE_ID,
        "refunded": {},
    }
    interaction = SimpleNamespace(
        guild=guild,
        response=SimpleNamespace(defer=AsyncMock()),
        followup=SimpleNamespace(send=AsyncMock()),
    )

    await MafiaCommands.admin_abort.callback(cog, interaction)

    channel.get_partial_message.assert_called_once_with(STANDINGS_MESSAGE_ID)
    channel.fetch_message.assert_not_awaited()
    board.unpin.assert_awaited_once()


@pytest.mark.asyncio
async def test_resolution_unpins_partial_standings_message(
    cog, game, guild, channel, board, monkeypatch
):
    monkeypatch.setattr(mafia_commands, "_mafia_post_channel", lambda _guild: channel)
    game.phase = MafiaPhase.RESOLVED

    await cog._post_resolution(
        guild,
        game,
        {
            "winner": "TOWN",
            "winning_ids": [],
            "vote_breakdown": {},
        },
    )

    channel.get_partial_message.assert_called_once_with(STANDINGS_MESSAGE_ID)
    channel.fetch_message.assert_not_awaited()
    board.unpin.assert_awaited_once()


@pytest.mark.asyncio
async def test_setup_overlaps_routes_and_waits_for_bounded_member_adds(
    cog,
    game,
    guild,
    mafia_service,
    flavor_service,
    monkeypatch,
):
    route_release = asyncio.Event()
    member_release = asyncio.Event()
    announcement_started = asyncio.Event()
    thread_create_started = asyncio.Event()
    all_members_started = asyncio.Event()
    order = []
    active_adds = 0
    peak_adds = 0
    add_attempts = 0

    players = [
        MafiaPlayer(
            game_id=GAME_ID,
            discord_id=discord_id,
            guild_id=GUILD_ID,
            role=MafiaRole.MAFIA,
            is_godfather=discord_id == 1,
        )
        for discord_id in range(1, 5)
    ]
    mafia_service.repo.get_players.return_value = players
    flavor_service.setup_narration = AsyncMock(return_value="The town sleeps.")
    game.roster_size = len(players)

    async def add_user(member):
        nonlocal active_adds, peak_adds, add_attempts
        active_adds += 1
        peak_adds = max(peak_adds, active_adds)
        add_attempts += 1
        order.append(f"member_started:{member.id}")
        if add_attempts == len(players):
            all_members_started.set()

        try:
            if member.id == 4:
                raise discord.HTTPException(
                    SimpleNamespace(status=500, reason="error"),
                    "member add failed",
                )
            await member_release.wait()
        finally:
            order.append(f"member_done:{member.id}")
            active_adds -= 1

    async def send_intro(content):
        order.append("intro")
        assert sum(item.startswith("member_done:") for item in order) == len(players)

    thread = SimpleNamespace(
        id=700,
        add_user=AsyncMock(side_effect=add_user),
        send=AsyncMock(side_effect=send_intro),
    )

    async def send_announcement(*, embed):
        order.append("announcement_started")
        announcement_started.set()
        await route_release.wait()
        order.append("announcement_done")
        return SimpleNamespace(id=600)

    async def create_thread(**kwargs):
        order.append("thread_create_started")
        thread_create_started.set()
        await route_release.wait()
        order.append("thread_created")
        return thread

    post_channel = SimpleNamespace(
        send=AsyncMock(side_effect=send_announcement),
        create_thread=AsyncMock(side_effect=create_thread),
    )
    guild.get_member = MagicMock(
        side_effect=lambda discord_id: SimpleNamespace(id=discord_id)
    )
    monkeypatch.setattr(
        mafia_commands, "_mafia_post_channel", lambda _guild: post_channel
    )
    monkeypatch.setattr(mafia_commands.asyncio, "to_thread", _run_sync)

    setup_task = asyncio.create_task(cog._post_setup(guild, game))
    await asyncio.wait_for(
        asyncio.gather(
            announcement_started.wait(),
            thread_create_started.wait(),
        ),
        timeout=1,
    )

    # The two independent channel routes started together.
    assert not setup_task.done()
    thread.add_user.assert_not_awaited()

    route_release.set()
    await asyncio.wait_for(all_members_started.wait(), timeout=1)

    # All four adds are in flight, but the intro remains ordered behind them.
    assert peak_adds == 4
    thread.send.assert_not_awaited()
    assert not setup_task.done()

    member_release.set()
    await setup_task

    assert thread.add_user.await_count == len(players)
    thread.send.assert_awaited_once()
    assert order.index("intro") > max(
        index for index, item in enumerate(order) if item.startswith("member_done:")
    )
    mafia_service.repo.set_thread_ids.assert_has_calls(
        [
            call(GAME_ID, setup_message_id=600),
            call(GAME_ID, mafia_thread_id=700),
        ],
        any_order=True,
    )
    assert cog._was_announced(GUILD_ID, game.game_date, MafiaPhase.SETUP)


@pytest.mark.asyncio
async def test_graveyard_member_adds_are_bounded_to_four(
    cog,
    game,
    guild,
    mafia_service,
    monkeypatch,
):
    member_release = asyncio.Event()
    first_wave_started = asyncio.Event()
    active_adds = 0
    peak_adds = 0
    add_attempts = 0
    dead_players = [
        MafiaPlayer(
            game_id=GAME_ID,
            discord_id=discord_id,
            guild_id=GUILD_ID,
            role=MafiaRole.TOWNIE,
            is_alive=False,
        )
        for discord_id in range(1, 10)
    ]
    mafia_service.repo.get_players.return_value = dead_players
    game.graveyard_thread_id = 800

    async def add_user(member):
        nonlocal active_adds, peak_adds, add_attempts
        active_adds += 1
        peak_adds = max(peak_adds, active_adds)
        add_attempts += 1
        if add_attempts == mafia_commands.THREAD_MEMBER_CONCURRENCY:
            first_wave_started.set()
        try:
            await member_release.wait()
        finally:
            active_adds -= 1

    thread = SimpleNamespace(add_user=AsyncMock(side_effect=add_user))
    guild.get_thread.return_value = thread
    guild.get_member = MagicMock(
        side_effect=lambda discord_id: SimpleNamespace(id=discord_id)
    )
    monkeypatch.setattr(
        mafia_commands,
        "_mafia_post_channel",
        lambda _guild: SimpleNamespace(),
    )
    monkeypatch.setattr(mafia_commands.asyncio, "to_thread", _run_sync)

    sync_task = asyncio.create_task(cog._sync_graveyard(guild, game))
    await asyncio.wait_for(first_wave_started.wait(), timeout=1)
    await asyncio.sleep(0)

    assert thread.add_user.await_count == mafia_commands.THREAD_MEMBER_CONCURRENCY
    assert peak_adds == mafia_commands.THREAD_MEMBER_CONCURRENCY
    assert not sync_task.done()

    member_release.set()
    await sync_task

    assert thread.add_user.await_count == len(dead_players)
    assert peak_adds == mafia_commands.THREAD_MEMBER_CONCURRENCY
