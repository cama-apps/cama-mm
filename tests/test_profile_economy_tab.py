"""Integration tests for the Economy tab on ``/profile`` — the balance-history chart wiring."""

from __future__ import annotations

import pytest

from commands.profile import ProfileCommands
from database import Database
from repositories.bet_repository import BetRepository
from repositories.disburse_repository import DisburseRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from repositories.tip_repository import TipRepository
from services.balance_history_service import BalanceHistoryService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def temp_db_path(tmp_path):
    """Temporary database with schema initialized."""
    db_path = str(tmp_path / "test_profile_economy.db")
    Database(db_path)
    return db_path


@pytest.fixture
def player_repo(temp_db_path):
    return PlayerRepository(temp_db_path)


@pytest.fixture
def match_repo(temp_db_path):
    return MatchRepository(temp_db_path)


@pytest.fixture
def bet_repo(temp_db_path):
    return BetRepository(temp_db_path)


@pytest.fixture
def prediction_repo(temp_db_path):
    return PredictionRepository(temp_db_path)


@pytest.fixture
def disburse_repo(temp_db_path):
    return DisburseRepository(temp_db_path)


@pytest.fixture
def tip_repo(temp_db_path):
    return TipRepository(temp_db_path)


@pytest.fixture
def balance_history_service(
    bet_repo, match_repo, player_repo, prediction_repo, disburse_repo, tip_repo
):
    return BalanceHistoryService(
        bet_repo=bet_repo,
        match_repo=match_repo,
        player_repo=player_repo,
        prediction_repo=prediction_repo,
        disburse_repo=disburse_repo,
        tip_repo=tip_repo,
    )


class MockUser:
    def __init__(self, user_id: int, display_name: str = "TestPlayer"):
        self.id = user_id
        self.display_name = display_name


class MockBot:
    """Only the attributes the Economy builder actually reads."""

    def __init__(self, *, player_repo, balance_history_service, tip_service=None):
        self.player_repo = player_repo
        self.balance_history_service = balance_history_service
        # Optional services — guarded by ``getattr(..., None)`` in the cog.
        self.tip_service = tip_service
        # Intentionally absent: loan_service, bankruptcy_service (both optional).


def _register_player(player_repo, discord_id: int):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
    )


@pytest.mark.asyncio
async def test_economy_tab_empty_history_returns_no_chart(
    player_repo, balance_history_service
):
    """A freshly-registered player has no chartable events — embed renders, no file."""
    discord_id = 100
    _register_player(player_repo, discord_id)

    bot = MockBot(player_repo=player_repo, balance_history_service=balance_history_service)
    cog = ProfileCommands(bot)
    user = MockUser(discord_id)

    embed, chart_file = await cog._build_economy_embed(user, discord_id, guild_id=TEST_GUILD_ID)

    assert chart_file is None
    field_names = {f.name for f in embed.fields}
    assert any("Balance" in name for name in field_names)
    assert not any("Balance History" in name for name in field_names)


@pytest.mark.asyncio
async def test_economy_tab_populated_history_attaches_chart(
    player_repo,
    match_repo,
    balance_history_service,
):
    """A player with recorded matches gets a chart file + a breakdown field."""
    discord_id = 200
    other_ids = [201, 202, 203, 204]
    for pid in [discord_id, *other_ids]:
        _register_player(player_repo, pid)

    # Record two matches: one win, one loss. Match-bonus reconstruction emits
    # two events (participation + win combined), enough to trip the chart.
    match_repo.record_match(
        team1_ids=[discord_id, 201, 202, 203, 204],
        team2_ids=[205, 206, 207, 208, 209],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    match_repo.record_match(
        team1_ids=[discord_id, 201, 202, 203, 204],
        team2_ids=[205, 206, 207, 208, 209],
        winning_team=2,
        guild_id=TEST_GUILD_ID,
    )

    bot = MockBot(player_repo=player_repo, balance_history_service=balance_history_service)
    cog = ProfileCommands(bot)
    user = MockUser(discord_id)

    embed, chart_file = await cog._build_economy_embed(user, discord_id, guild_id=TEST_GUILD_ID)

    assert chart_file is not None
    assert chart_file.filename == "balance_history.png"
    field_names = {f.name for f in embed.fields}
    assert any("Balance History" in name for name in field_names)


@pytest.mark.asyncio
async def test_economy_tab_unregistered_player_short_circuits(
    player_repo, balance_history_service
):
    """Unregistered player: 'Not Registered' embed and no chart."""
    bot = MockBot(player_repo=player_repo, balance_history_service=balance_history_service)
    cog = ProfileCommands(bot)
    user = MockUser(999999, "Ghost")

    embed, chart_file = await cog._build_economy_embed(user, user.id, guild_id=TEST_GUILD_ID)

    assert chart_file is None
    assert "Not Registered" in (embed.title or "")


@pytest.mark.asyncio
async def test_economy_tab_without_balance_history_service(player_repo, match_repo):
    """If the bot doesn't have the service wired (legacy setup), the tab still renders."""
    discord_id = 300
    _register_player(player_repo, discord_id)
    match_repo.record_match(
        team1_ids=[discord_id, 301, 302, 303, 304],
        team2_ids=[305, 306, 307, 308, 309],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )

    bot = MockBot(player_repo=player_repo, balance_history_service=None)
    cog = ProfileCommands(bot)
    user = MockUser(discord_id)

    embed, chart_file = await cog._build_economy_embed(user, discord_id, guild_id=TEST_GUILD_ID)

    assert chart_file is None
    assert "Economy" in (embed.title or "")
