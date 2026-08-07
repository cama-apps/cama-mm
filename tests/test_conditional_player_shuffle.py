"""Regression coverage for the deprecated Frogling conditional queue."""

from types import SimpleNamespace
from unittest.mock import ANY, AsyncMock, MagicMock

import pytest

from commands.match import MatchCommands, select_players_for_shuffle
from domain.models.lobby import LobbyKind
from domain.models.player import Player
from tests.conftest import TEST_GUILD_ID


def test_shuffle_roster_ignores_legacy_conditional_players():
    regular_ids = list(range(1, 10))
    regular_players = [object() for _ in regular_ids]

    player_ids, players, included, excluded = select_players_for_shuffle(
        regular_ids,
        regular_players,
        [99, 100],
        [
            SimpleNamespace(glicko_rating=1500.0, glicko_rd=350.0),
            SimpleNamespace(glicko_rating=1400.0, glicko_rd=350.0),
        ],
    )

    assert player_ids == regular_ids
    assert players == regular_players
    assert included == []
    assert excluded == []


@pytest.mark.asyncio
async def test_readycheck_nonresponders_are_implicit_conditional_exclusions():
    player_ids = list(range(1, 16))
    players = [
        Player(name=f"Player {player_id}", discord_id=player_id)
        for player_id in player_ids
    ]
    confirmed_ids = set(player_ids[:10])
    join_times = {player_id: 1_000.0 + player_id for player_id in player_ids}
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_lobby_players_and_readycheck_snapshot.return_value = (
        player_ids,
        players,
        join_times,
        (1234, confirmed_ids | {999}),
    )
    cog = MatchCommands(MagicMock(), lobby_service, MagicMock(), MagicMock())

    selected_ids, selected_players, included, excluded, selected_join_times = (
        await cog._select_shuffle_roster(TEST_GUILD_ID)
    )

    assert selected_ids == player_ids[:10]
    assert [player.discord_id for player in selected_players] == player_ids[:10]
    assert included == []
    assert excluded == player_ids[10:]
    assert selected_join_times == {
        player_id: join_times[player_id] for player_id in player_ids[:10]
    }


@pytest.mark.asyncio
async def test_readycheck_below_threshold_does_not_change_shuffle_roster():
    player_ids = list(range(1, 13))
    players = [
        Player(name=f"Player {player_id}", discord_id=player_id)
        for player_id in player_ids
    ]
    join_times = {player_id: 2_000.0 + player_id for player_id in player_ids}
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_lobby_players_and_readycheck_snapshot.return_value = (
        player_ids,
        players,
        join_times,
        (1234, set(player_ids[:9]) | {999}),
    )
    cog = MatchCommands(MagicMock(), lobby_service, MagicMock(), MagicMock())

    selected_ids, selected_players, included, excluded, selected_join_times = (
        await cog._select_shuffle_roster(TEST_GUILD_ID)
    )

    assert selected_ids == player_ids
    assert selected_players == players
    assert included == []
    assert excluded == []
    assert selected_join_times == join_times


@pytest.mark.asyncio
async def test_execute_shuffle_stops_if_lobby_changes_before_atomic_snapshot(
    monkeypatch,
):
    lobby = SimpleNamespace(get_player_count=lambda: 10)
    match_service = MagicMock()
    interaction = SimpleNamespace(followup=SimpleNamespace(send=AsyncMock()))
    cog = MatchCommands(MagicMock(), MagicMock(), match_service, MagicMock())
    monkeypatch.setattr(
        cog,
        "_validate_shuffle_preconditions",
        AsyncMock(return_value=lobby),
    )
    monkeypatch.setattr(cog, "_select_shuffle_roster", AsyncMock(return_value=None))

    await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    match_service.shuffle_players.assert_not_called()
    interaction.followup.send.assert_awaited_once_with(
        "❌ The lobby changed while starting the shuffle. Please try again.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_execute_shuffle_passes_no_conditional_exclusions_to_match_service(monkeypatch):
    player_ids = list(range(100, 110))
    shuffle_time = 10_000.0
    join_times = {
        100: shuffle_time - (30 * 60 + 59),
        101: shuffle_time - 59,
        102: shuffle_time + 60,
    }
    lobby = SimpleNamespace(get_player_count=lambda: 10)
    match_service = MagicMock()
    match_service.state_service.get_all_pending_player_ids.return_value = set()
    match_service.shuffle_players.side_effect = RuntimeError("stop after service call")
    interaction = SimpleNamespace(followup=SimpleNamespace(send=AsyncMock()))
    lobby_service = MagicMock()
    cog = MatchCommands(MagicMock(), lobby_service, match_service, MagicMock())
    monkeypatch.setattr(
        "commands.match.time",
        SimpleNamespace(time=lambda: shuffle_time),
    )
    monkeypatch.setattr(cog, "_validate_shuffle_preconditions", AsyncMock(return_value=lobby))
    monkeypatch.setattr(
        cog,
        "_select_shuffle_roster",
        AsyncMock(return_value=(player_ids, [], [], [], join_times)),
    )

    await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    match_service.shuffle_players.assert_called_once_with(
        player_ids,
        guild_id=TEST_GUILD_ID,
        betting_mode="pool",
        rating_system=ANY,
        shuffle_mode="balanced",
        excluded_conditional_ids=[],
        lobby_wait_minutes={100: 30, 101: 0, 102: 0},
        lobby_kind=LobbyKind.OPEN,
    )
    lobby_service.reserve_lobby_players.assert_called_once_with(
        player_ids,
        TEST_GUILD_ID,
        LobbyKind.OPEN,
    )
    lobby_service.release_lobby_players.assert_called_once_with(
        player_ids,
        TEST_GUILD_ID,
        LobbyKind.OPEN,
    )


@pytest.mark.asyncio
async def test_execute_shuffle_refreshes_pool_exactly_once_on_later_failure(monkeypatch):
    player_ids = list(range(100, 110))
    lobby = SimpleNamespace(get_player_count=lambda: 10)
    match_service = MagicMock()
    match_service.state_service.get_all_pending_player_ids.return_value = set()
    match_service.shuffle_players.return_value = {}
    interaction = SimpleNamespace(followup=SimpleNamespace(send=AsyncMock()))
    lobby_service = MagicMock()
    cog = MatchCommands(MagicMock(), lobby_service, match_service, MagicMock())
    refresh_pool_lobbies = AsyncMock()
    monkeypatch.setattr(
        cog,
        "_validate_shuffle_preconditions",
        AsyncMock(return_value=lobby),
    )
    monkeypatch.setattr(
        cog,
        "_select_shuffle_roster",
        AsyncMock(return_value=(player_ids, [], [], [], {})),
    )
    monkeypatch.setattr(
        cog,
        "_refresh_bonus_pool_lobbies",
        refresh_pool_lobbies,
        raising=False,
    )

    with pytest.raises(KeyError, match="radiant_team"):
        await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    refresh_pool_lobbies.assert_awaited_once_with(TEST_GUILD_ID)
    lobby_service.release_lobby_players.assert_called_once_with(
        player_ids,
        TEST_GUILD_ID,
        LobbyKind.OPEN,
    )


@pytest.mark.asyncio
async def test_execute_shuffle_refreshes_pool_exactly_once_after_finalize(monkeypatch):
    """On success the pool refresh runs once, after the lobby-resetting finalize."""
    player_ids = list(range(100, 110))
    lobby = SimpleNamespace(get_player_count=lambda: 10)
    team_players = [
        SimpleNamespace(get_value=lambda *args, **kwargs: 100.0) for _ in range(5)
    ]
    team = SimpleNamespace(players=team_players, get_off_role_count=lambda: 0)
    match_service = MagicMock()
    match_service.state_service.get_all_pending_player_ids.return_value = set()
    match_service.shuffle_players.return_value = {
        "radiant_team": team,
        "dire_team": team,
        "radiant_roles": ["1", "2", "3", "4", "5"],
        "dire_roles": ["1", "2", "3", "4", "5"],
        "value_diff": 0.0,
        "first_pick_team": "Radiant",
        "excluded_ids": [],
        "pending_match_id": 7,
    }
    bot = MagicMock()
    bot.betting_service = None
    interaction = SimpleNamespace(followup=SimpleNamespace(send=AsyncMock()))
    cog = MatchCommands(bot, MagicMock(), match_service, MagicMock())
    order = []
    finalize = AsyncMock(side_effect=lambda **kwargs: order.append("finalize"))
    refresh_pool_lobbies = AsyncMock(side_effect=lambda _gid: order.append("refresh"))
    monkeypatch.setattr(
        cog,
        "_validate_shuffle_preconditions",
        AsyncMock(return_value=lobby),
    )
    monkeypatch.setattr(
        cog,
        "_select_shuffle_roster",
        AsyncMock(return_value=(player_ids, [], [], [], {})),
    )
    monkeypatch.setattr(cog, "_format_team_lines", lambda *args, **kwargs: ["line"] * 5)
    monkeypatch.setattr("commands.match.summarize_region", lambda players: "USE")
    monkeypatch.setattr(cog, "_finalize_shuffle", finalize, raising=False)
    monkeypatch.setattr(
        cog,
        "_refresh_bonus_pool_lobbies",
        refresh_pool_lobbies,
        raising=False,
    )

    await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    finalize.assert_awaited_once()
    refresh_pool_lobbies.assert_awaited_once_with(TEST_GUILD_ID)
    assert order == ["finalize", "refresh"]


@pytest.mark.asyncio
async def test_readycheck_nonresponders_are_forwarded_to_normal_shuffle(monkeypatch):
    player_ids = list(range(100, 110))
    excluded_ids = list(range(110, 115))
    lobby = SimpleNamespace(get_player_count=lambda: 15)
    bot = MagicMock()
    match_service = MagicMock()
    match_service.state_service.get_all_pending_player_ids.return_value = set()
    match_service.shuffle_players.side_effect = RuntimeError("stop after service call")
    interaction = SimpleNamespace(followup=SimpleNamespace(send=AsyncMock()))
    cog = MatchCommands(bot, MagicMock(), match_service, MagicMock())
    monkeypatch.setattr(cog, "_validate_shuffle_preconditions", AsyncMock(return_value=lobby))
    monkeypatch.setattr(
        cog,
        "_select_shuffle_roster",
        AsyncMock(return_value=(player_ids, [], [], excluded_ids, {})),
    )

    await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    match_service.shuffle_players.assert_called_once_with(
        player_ids,
        guild_id=TEST_GUILD_ID,
        betting_mode="pool",
        rating_system=ANY,
        shuffle_mode="balanced",
        excluded_conditional_ids=excluded_ids,
        lobby_wait_minutes={},
        lobby_kind=LobbyKind.OPEN,
    )


@pytest.mark.asyncio
async def test_execute_shuffle_reuses_loaded_roster_for_pending_names(monkeypatch):
    player_ids = list(range(100, 110))
    players = [Player(name=f"Player {player_id}", discord_id=player_id) for player_id in player_ids]
    lobby = SimpleNamespace(get_player_count=lambda: 10)
    match_service = MagicMock()
    match_service.state_service.get_all_pending_player_ids.return_value = {100}
    player_service = MagicMock()
    interaction = SimpleNamespace(followup=SimpleNamespace(send=AsyncMock()))
    cog = MatchCommands(MagicMock(), MagicMock(), match_service, player_service)
    monkeypatch.setattr(cog, "_validate_shuffle_preconditions", AsyncMock(return_value=lobby))
    monkeypatch.setattr(
        cog,
        "_select_shuffle_roster",
        AsyncMock(return_value=(player_ids, players, [], [], {})),
    )

    await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    player_service.get_player.assert_not_called()
    sent = interaction.followup.send.await_args.args[0]
    assert "Player 100" in sent
