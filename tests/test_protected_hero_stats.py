from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.profile import ProfileCommands
from commands.shop import SHOP_PROTECT_HERO_COST, ShopCommands
from domain.models.pending_match_state import PendingMatchState
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID


def _register(player_repo: PlayerRepository, discord_id: int, *, balance: int = 500) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"u{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def _record_match(
    match_repo: MatchRepository,
    team1: list[int],
    team2: list[int],
    *,
    pending_match_id: int | None = None,
    winning_team: int = 1,
) -> int:
    winning_ids = team1 if winning_team == 1 else team2
    losing_ids = team2 if winning_team == 1 else team1
    return match_repo.record_match_core_atomic(
        team1_ids=team1,
        team2_ids=team2,
        winning_team=winning_team,
        guild_id=TEST_GUILD_ID,
        dotabuff_match_id=None,
        lobby_type="shuffle",
        balancing_rating_system="glicko",
        winning_ids=winning_ids,
        losing_ids=losing_ids,
        glicko_updates=[],
        openskill_updates=[],
        rating_history_rows=[],
        match_prediction={
            "radiant_rating": 1500.0,
            "dire_rating": 1500.0,
            "radiant_rd": 100.0,
            "dire_rd": 100.0,
            "expected_radiant_win_prob": 0.5,
        },
        last_match_date_iso="2026-05-17T00:00:00+00:00",
        first_calibration_ids=[],
        first_calibration_unix=0,
        effective_avoid_ids=[],
        effective_deal_ids=[],
        pending_match_id=pending_match_id,
    )


def test_protected_hero_purchase_debits_atomically_and_blocks_duplicates(
    player_repository,
    match_repository,
):
    _register(player_repository, 1, balance=300)
    pending_match_id = match_repository.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": [1], "dire_team_ids": [2]},
    )

    result = match_repository.purchase_protected_hero_atomic(
        guild_id=TEST_GUILD_ID,
        pending_match_id=pending_match_id,
        discord_id=1,
        hero_id=1,
        team_side="radiant",
        cost=SHOP_PROTECT_HERO_COST,
    )

    assert result["success"] is True
    assert result["balance_after"] == 300 - SHOP_PROTECT_HERO_COST
    assert player_repository.get_balance(1, TEST_GUILD_ID) == 300 - SHOP_PROTECT_HERO_COST

    duplicate = match_repository.purchase_protected_hero_atomic(
        guild_id=TEST_GUILD_ID,
        pending_match_id=pending_match_id,
        discord_id=1,
        hero_id=2,
        team_side="radiant",
        cost=SHOP_PROTECT_HERO_COST,
    )

    assert duplicate["success"] is False
    assert duplicate["reason"] == "already_protected"
    assert duplicate["hero_id"] == 1
    assert player_repository.get_balance(1, TEST_GUILD_ID) == 300 - SHOP_PROTECT_HERO_COST


def test_protected_hero_stats_count_confirmed_enriched_games(
    player_repository,
    match_repository,
):
    team1, team2 = [1, 2, 3, 4, 5], [6, 7, 8, 9, 10]
    for discord_id in team1 + team2:
        _register(player_repository, discord_id)
    pending_match_id = match_repository.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": team1, "dire_team_ids": team2},
    )
    match_repository.purchase_protected_hero_atomic(
        guild_id=TEST_GUILD_ID,
        pending_match_id=pending_match_id,
        discord_id=1,
        hero_id=1,
        team_side="radiant",
        cost=SHOP_PROTECT_HERO_COST,
    )

    match_id = _record_match(
        match_repository,
        team1,
        team2,
        pending_match_id=pending_match_id,
        winning_team=1,
    )
    match_repository.update_participant_stats_bulk(match_id, [{"discord_id": 1, "hero_id": 1}])

    stats = match_repository.get_player_protected_hero_stats(1, TEST_GUILD_ID)

    assert stats["attempts"] == 1
    assert stats["confirmed_games"] == 1
    assert stats["wins"] == 1
    assert stats["losses"] == 0
    assert stats["not_played_games"] == 0
    assert stats["unenriched_games"] == 0
    assert stats["top_heroes"] == [{"hero_id": 1, "games": 1, "wins": 1}]


def test_protected_hero_stats_track_not_played_and_unenriched_games(
    player_repository,
    match_repository,
):
    team1, team2 = [1, 2, 3, 4, 5], [6, 7, 8, 9, 10]
    for discord_id in team1 + team2:
        _register(player_repository, discord_id)

    mismatch_pending_id = match_repository.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": team1, "dire_team_ids": team2},
    )
    match_repository.purchase_protected_hero_atomic(
        guild_id=TEST_GUILD_ID,
        pending_match_id=mismatch_pending_id,
        discord_id=1,
        hero_id=1,
        team_side="radiant",
        cost=SHOP_PROTECT_HERO_COST,
    )
    mismatch_match_id = _record_match(
        match_repository,
        team1,
        team2,
        pending_match_id=mismatch_pending_id,
    )
    match_repository.update_participant_stats_bulk(
        mismatch_match_id,
        [{"discord_id": 1, "hero_id": 2}],
    )

    unenriched_pending_id = match_repository.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": team1, "dire_team_ids": team2},
    )
    match_repository.purchase_protected_hero_atomic(
        guild_id=TEST_GUILD_ID,
        pending_match_id=unenriched_pending_id,
        discord_id=1,
        hero_id=3,
        team_side="radiant",
        cost=SHOP_PROTECT_HERO_COST,
    )
    _record_match(match_repository, team1, team2, pending_match_id=unenriched_pending_id)

    stats = match_repository.get_player_protected_hero_stats(1, TEST_GUILD_ID)

    assert stats["attempts"] == 2
    assert stats["confirmed_games"] == 0
    assert stats["not_played_games"] == 1
    assert stats["unenriched_games"] == 1


@pytest.mark.asyncio
async def test_protect_hero_command_uses_atomic_purchase_not_adjust_balance(monkeypatch):
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = 9999

    pending_state = PendingMatchState(
        radiant_team_ids=[1001, 1002],
        dire_team_ids=[1003, 1004],
        pending_match_id=55,
    )
    match_service = SimpleNamespace(
        get_pending_match_for_player=MagicMock(return_value=pending_state),
        get_last_shuffle=MagicMock(return_value=None),
        purchase_protected_hero=MagicMock(return_value={"success": True}),
    )
    bot = MagicMock()
    bot.get_channel.return_value = None
    commands = ShopCommands(bot, player_service, match_service=match_service)
    interaction = MagicMock()
    interaction.user = SimpleNamespace(id=1001, mention="<@1001>", display_name="Buyer")
    interaction.guild = SimpleNamespace(id=TEST_GUILD_ID)
    interaction.channel = None
    interaction.response = AsyncMock()
    interaction.followup = AsyncMock()

    monkeypatch.setattr("commands.shop.get_hero_image_url", lambda _hero_id: None)
    monkeypatch.setattr("commands.shop.get_hero_color", lambda _hero_id: None)

    await commands._handle_protect_hero(interaction, hero="1")

    match_service.purchase_protected_hero.assert_called_once_with(
        guild_id=TEST_GUILD_ID,
        pending_match_id=55,
        discord_id=1001,
        hero_id=1,
        team_side="radiant",
        cost=SHOP_PROTECT_HERO_COST,
    )
    player_service.adjust_balance.assert_not_called()
    player_service.get_balance.assert_not_called()


def test_profile_formats_protected_hero_stats():
    line = ProfileCommands._format_protected_hero_stats(
        {
            "confirmed_games": 3,
            "wins": 2,
            "losses": 1,
            "pending_purchases": 1,
            "not_played_games": 1,
            "unenriched_games": 0,
            "attempts": 4,
            "top_heroes": [{"hero_id": 1, "games": 3, "wins": 2}],
        },
        lambda hero_id: "Anti-Mage" if hero_id == 1 else "Unknown",
    )

    assert "**Protected:** 2W-1L (67%)" in line
    assert "**Best Protected:** Anti-Mage (3g, 67%)" in line
    assert "1 pending" in line
    assert "1 not played" in line
