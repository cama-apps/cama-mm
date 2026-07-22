"""Regression tests for the shared OpenDota fetch used by the profile Dota tab."""

from unittest.mock import Mock

import pytest

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
