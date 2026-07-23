"""Regression tests for the shared OpenDota fetch used by the profile Dota tab."""

import asyncio
from io import BytesIO
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

import commands.profile as profile_module
from commands.profile import ProfileCommands
from tests.conftest import TEST_GUILD_ID


class MockUser:
    def __init__(self, user_id: int, display_name: str = "TestPlayer"):
        self.id = user_id
        self.display_name = display_name


@pytest.mark.asyncio
async def test_dota_tab_uses_one_combined_stats_call():
    discord_id = 123
    steam_id = 7654321
    player_repo = Mock()
    player_repo.get_by_id.return_value = object()
    player_repo.get_steam_id.return_value = steam_id
    opendota_service = Mock()
    opendota_service.get_dota_tab_stats.return_value = {
        "role_distribution": None,
        "full_stats": None,
    }
    bot = Mock(
        player_repo=player_repo,
        opendota_player_service=opendota_service,
    )
    cog = ProfileCommands(bot)

    await cog._build_dota_embed(
        MockUser(discord_id),
        discord_id,
        guild_id=TEST_GUILD_ID,
    )

    opendota_service.get_dota_tab_stats.assert_called_once_with(
        discord_id,
        match_limit=50,
        steam_id=steam_id,
    )
    opendota_service.get_hero_role_distribution.assert_not_called()
    opendota_service.get_full_stats.assert_not_called()


@pytest.mark.asyncio
async def test_dota_tab_draws_independent_charts_concurrently(monkeypatch):
    active = 0
    peak = 0

    async def tracked_to_thread(function, /, *args, **kwargs):
        nonlocal active, peak
        active += 1
        peak = max(peak, active)
        await asyncio.sleep(0)
        try:
            return function(*args, **kwargs)
        finally:
            active -= 1

    async def direct_opendota(function, /, *args, **kwargs):
        return function(*args, **kwargs)

    monkeypatch.setattr(profile_module.asyncio, "to_thread", tracked_to_thread)
    monkeypatch.setattr(profile_module, "run_opendota_io", direct_opendota)
    role_draw = Mock(return_value=BytesIO(b"role"))
    lane_draw = Mock(return_value=BytesIO(b"lane"))
    monkeypatch.setattr(profile_module, "draw_role_graph", role_draw)
    monkeypatch.setattr(profile_module, "draw_lane_distribution", lane_draw)

    player_repo = SimpleNamespace(
        get_by_id=Mock(return_value=object()),
        get_steam_id=Mock(return_value=7654321),
    )
    opendota_service = SimpleNamespace(
        get_dota_tab_stats=Mock(
            return_value={
                "role_distribution": {"Carry": 100.0},
                "full_stats": {
                    "lane_distribution": {"Safe": 100.0},
                    "lane_parsed_count": 10,
                    "win_rate": None,
                    "avg_kills": 0,
                    "avg_deaths": 0,
                    "avg_assists": 0,
                    "hero_counts": [],
                },
            }
        )
    )
    cog = ProfileCommands(
        SimpleNamespace(
            player_repo=player_repo,
            opendota_player_service=opendota_service,
        )
    )

    embed, files = await cog._build_dota_embed(
        MockUser(123),
        123,
        guild_id=TEST_GUILD_ID,
    )

    try:
        assert peak == 2
        assert [file.filename for file in files] == [
            "role_graph.png",
            "lane_graph.png",
        ]
        assert embed.image.url == "attachment://role_graph.png"
        role_draw.assert_called_once()
        lane_draw.assert_called_once_with({"Safe": 100.0})
    finally:
        for file in files:
            file.close()
