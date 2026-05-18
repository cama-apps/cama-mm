"""Integration tests for the Predictions tab on ``/profile`` — order-book wiring."""

from __future__ import annotations

import pytest

from commands.profile import ProfileCommands
from config import PREDICTION_CONTRACT_VALUE
from database import Database
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from services.prediction_service import PredictionService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def temp_db_path(tmp_path):
    """Temporary database with schema initialized."""
    db_path = str(tmp_path / "test_profile_predictions.db")
    Database(db_path)
    return db_path


@pytest.fixture
def player_repo(temp_db_path):
    return PlayerRepository(temp_db_path)


@pytest.fixture
def prediction_repo(temp_db_path):
    return PredictionRepository(temp_db_path)


@pytest.fixture
def prediction_service(prediction_repo, player_repo):
    return PredictionService(
        prediction_repo=prediction_repo, player_repo=player_repo, admin_user_ids=[],
    )


class MockUser:
    def __init__(self, user_id: int, display_name: str = "TestPlayer"):
        self.id = user_id
        self.display_name = display_name


class MockBot:
    """Only the attribute the Predictions builder actually reads."""

    def __init__(self, prediction_service):
        self.prediction_service = prediction_service


def _register_player(player_repo, discord_id: int, balance: int = 100_000):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def _field(embed, name_contains: str):
    """Return the first embed field whose name contains the substring, or None."""
    for f in embed.fields:
        if name_contains in f.name:
            return f
    return None


@pytest.mark.asyncio
async def test_predictions_tab_empty(player_repo, prediction_service):
    """A player with no order-book activity gets the empty-state embed."""
    _register_player(player_repo, 100)
    cog = ProfileCommands(MockBot(prediction_service))

    embed, file = await cog._build_predictions_embed(
        MockUser(100), 100, guild_id=TEST_GUILD_ID
    )

    assert file is None
    assert not embed.fields
    assert "/predict list" in (embed.description or "")


@pytest.mark.asyncio
async def test_predictions_tab_open_position(player_repo, prediction_service):
    """An open position is shown under Open Positions; the record is 0W-0L."""
    _register_player(player_repo, 200)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=200, question="Will it rain?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=200, side="yes", contracts=3,
    )
    cog = ProfileCommands(MockBot(prediction_service))

    embed, file = await cog._build_predictions_embed(
        MockUser(200), 200, guild_id=TEST_GUILD_ID
    )

    positions = _field(embed, "Open Positions")
    assert positions is not None
    assert f"#{pid}" in positions.value
    assert "0W-0L" in _field(embed, "Performance").value


@pytest.mark.asyncio
async def test_predictions_tab_resolved_win(
    player_repo, prediction_service, prediction_repo
):
    """A resolved win shows positive realized P&L and a 1W-0L record."""
    _register_player(player_repo, 300)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=300, question="Will it snow?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=300, side="yes", contracts=3,
    )
    cost = prediction_repo.get_position(pid, 300)["yes_cost_basis_total"]
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")
    cog = ProfileCommands(MockBot(prediction_service))

    embed, _ = await cog._build_predictions_embed(
        MockUser(300), 300, guild_id=TEST_GUILD_ID
    )

    performance = _field(embed, "Performance")
    assert performance is not None
    assert f"+{3 * PREDICTION_CONTRACT_VALUE - cost}" in performance.value
    assert "1W-0L" in performance.value


@pytest.mark.asyncio
async def test_predictions_tab_resolved_loss(
    player_repo, prediction_service, prediction_repo
):
    """A resolved loss shows negative realized P&L and a 0W-1L record."""
    _register_player(player_repo, 400)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=400, question="Will it hail?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=400, side="yes", contracts=3,
    )
    cost = prediction_repo.get_position(pid, 400)["yes_cost_basis_total"]
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="no")
    cog = ProfileCommands(MockBot(prediction_service))

    embed, _ = await cog._build_predictions_embed(
        MockUser(400), 400, guild_id=TEST_GUILD_ID
    )

    performance = _field(embed, "Performance")
    assert performance is not None
    assert str(-cost) in performance.value
    assert "0W-1L" in performance.value
