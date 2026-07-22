"""Regression coverage for the deprecated Frogling conditional queue."""

from types import SimpleNamespace
from unittest.mock import ANY, AsyncMock, MagicMock

import pytest

from commands.match import MatchCommands, select_players_for_shuffle
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
async def test_execute_shuffle_passes_no_conditional_exclusions_to_match_service(monkeypatch):
    player_ids = list(range(100, 110))
    lobby = SimpleNamespace(get_player_count=lambda: 10)
    match_service = MagicMock()
    match_service.state_service.get_all_pending_player_ids.return_value = set()
    match_service.shuffle_players.side_effect = RuntimeError("stop after service call")
    interaction = SimpleNamespace(followup=SimpleNamespace(send=AsyncMock()))
    cog = MatchCommands(MagicMock(), MagicMock(), match_service, MagicMock())
    monkeypatch.setattr(cog, "_validate_shuffle_preconditions", AsyncMock(return_value=lobby))
    monkeypatch.setattr(
        cog,
        "_select_shuffle_roster",
        AsyncMock(return_value=(player_ids, [], [], [])),
    )

    await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    match_service.shuffle_players.assert_called_once_with(
        player_ids,
        guild_id=TEST_GUILD_ID,
        betting_mode="pool",
        rating_system=ANY,
        shuffle_mode="balanced",
        excluded_conditional_ids=[],
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
        AsyncMock(return_value=(player_ids, players, [], [])),
    )

    await cog._execute_shuffle(interaction, None, TEST_GUILD_ID, None)

    player_service.get_player.assert_not_called()
    sent = interaction.followup.send.await_args.args[0]
    assert "Player 100" in sent
