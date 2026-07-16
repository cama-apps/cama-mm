"""The Overview tab on /profile surfaces lottery activity status."""

from __future__ import annotations

import pytest

from commands.profile import ProfileCommands
from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


@pytest.fixture
def player_service(player_repo):
    return PlayerService(player_repo)


class MockUser:
    def __init__(self, user_id: int, display_name: str = "TestPlayer"):
        self.id = user_id
        self.display_name = display_name


class MockBot:
    """Only the attributes the Overview builder actually reads."""

    def __init__(self, *, player_repo, player_service):
        self.player_repo = player_repo
        self.player_service = player_service
        # Intentionally absent (all optional / guarded): bankruptcy_service,
        # mana_effects_service, match_repo, match_service.


def _register(player_repo, discord_id: int):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
    )


def _lottery_field(embed):
    return next((f.value for f in embed.fields if f.name == "Lottery"), None)


@pytest.mark.asyncio
async def test_overview_shows_active(player_repo, player_service):
    """A player with recent channel activity shows the active badge."""
    discord_id = 100
    _register(player_repo, discord_id)
    player_repo.bump_last_active_many([discord_id], TEST_GUILD_ID)

    cog = ProfileCommands(MockBot(player_repo=player_repo, player_service=player_service))
    embed, _ = await cog._build_overview_embed(MockUser(discord_id), discord_id, guild_id=TEST_GUILD_ID)

    assert "Active" in (_lottery_field(embed) or "")
    assert "Inactive" not in (_lottery_field(embed) or "")


@pytest.mark.asyncio
async def test_overview_shows_inactive(player_repo, player_service):
    """A player with no recent activity shows the inactive badge."""
    discord_id = 200
    _register(player_repo, discord_id)

    cog = ProfileCommands(MockBot(player_repo=player_repo, player_service=player_service))
    embed, _ = await cog._build_overview_embed(MockUser(discord_id), discord_id, guild_id=TEST_GUILD_ID)

    assert "Inactive" in (_lottery_field(embed) or "")
