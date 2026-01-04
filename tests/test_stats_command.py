"""
Tests for the /stats command behavior with user mentions.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

from commands.registration import RegistrationCommands
from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService


class FakeInteraction:
    """Minimal interaction stub to satisfy stats command dependencies."""

    def __init__(self, user_id: int, user_name: str = "SelfUser"):
        self.user = SimpleNamespace(
            id=user_id,
            mention=f"<@{user_id}>",
            __str__=lambda self: user_name,
        )
        self.response = SimpleNamespace(defer=AsyncMock(), is_done=lambda: False)
        self.followup = SimpleNamespace(send=AsyncMock())
        self.channel = None
        self.guild = None


@pytest.mark.asyncio
async def test_stats_command_targets_mentioned_user(repo_db_path):
    """Ensure /stats uses the provided user mention instead of the caller."""
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(
        discord_id=123,
        discord_username="SelfUser",
        initial_mmr=2000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.add(
        discord_id=456,
        discord_username="TargetUser",
        initial_mmr=2100,
        glicko_rating=1525.0,
        glicko_rd=340.0,
        glicko_volatility=0.06,
    )

    player_service = PlayerService(player_repo)
    bot = Mock()
    commands_cog = RegistrationCommands(
        bot, db=None, player_service=player_service, role_emojis={}, role_names={}
    )

    interaction = FakeInteraction(user_id=123, user_name="SelfUser")
    target_member = SimpleNamespace(id=456, mention="<@456>", display_name="TargetUser")

    # app_commands turn methods into Command objects; call the underlying callback directly
    await RegistrationCommands.stats.callback(commands_cog, interaction, user=target_member)

    interaction.response.defer.assert_awaited_once()
    interaction.followup.send.assert_awaited_once()

    args, kwargs = interaction.followup.send.call_args
    assert "embed" in kwargs
    embed = kwargs["embed"]
    assert embed.title == "📊 Stats for TargetUser"
