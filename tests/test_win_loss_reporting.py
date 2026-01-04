"""
Unit tests for win/loss reporting and stats calculation.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

from commands.registration import RegistrationCommands
from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService


def _set_wins_losses(repo: PlayerRepository, discord_id: int, wins: int, losses: int) -> None:
    with repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE players SET wins = ?, losses = ? WHERE discord_id = ?",
            (wins, losses, discord_id),
        )


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


def _add_player(repo: PlayerRepository, discord_id: int, username: str = None) -> None:
    repo.add(
        discord_id=discord_id,
        discord_username=username or f"Player{discord_id}",
        initial_mmr=2000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )


def test_get_stats_win_rate_variations(player_repo):
    player_id = 71001
    _add_player(player_repo, player_id)
    service = PlayerService(player_repo)

    # No games -> win_rate None
    stats = service.get_stats(player_id)
    assert stats["win_rate"] is None

    # 100% win rate
    _set_wins_losses(player_repo, player_id, wins=5, losses=0)
    stats = service.get_stats(player_id)
    assert stats["win_rate"] == pytest.approx(100.0)

    # 0% win rate
    _set_wins_losses(player_repo, player_id, wins=0, losses=4)
    stats = service.get_stats(player_id)
    assert stats["win_rate"] == pytest.approx(0.0)

    # 50% win rate
    _set_wins_losses(player_repo, player_id, wins=3, losses=3)
    stats = service.get_stats(player_id)
    assert stats["win_rate"] == pytest.approx(50.0)


class FakeInteraction:
    """Minimal interaction stub to inspect stats embed output."""

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
async def test_stats_command_includes_win_loss_and_win_rate(player_repo):
    requester_id = 72001
    target_id = 72002
    _add_player(player_repo, requester_id, "Requester")
    _add_player(player_repo, target_id, "TargetUser")
    _set_wins_losses(player_repo, target_id, wins=4, losses=1)  # 80% win rate

    player_service = PlayerService(player_repo)
    bot = Mock()
    commands_cog = RegistrationCommands(
        bot, db=None, player_service=player_service, role_emojis={}, role_names={}
    )

    interaction = FakeInteraction(user_id=requester_id, user_name="Requester")
    target_member = SimpleNamespace(
        id=target_id, mention=f"<@{target_id}>", display_name="TargetUser"
    )

    await RegistrationCommands.stats.callback(commands_cog, interaction, user=target_member)

    interaction.response.defer.assert_awaited_once()
    interaction.followup.send.assert_awaited_once()

    _, kwargs = interaction.followup.send.call_args
    embed = kwargs["embed"]

    # Field order: Cama Rating, Wins, Losses, Win Rate, Jopacoin Balance
    wins_field = next((f for f in embed.fields if f.name == "Wins"), None)
    losses_field = next((f for f in embed.fields if f.name == "Losses"), None)
    win_rate_field = next((f for f in embed.fields if f.name == "Win Rate"), None)

    assert wins_field is not None and wins_field.value == "4"
    assert losses_field is not None and losses_field.value == "1"
    assert win_rate_field is not None and "80.0%" in win_rate_field.value
