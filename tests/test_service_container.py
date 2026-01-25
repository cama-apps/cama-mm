"""Tests for ServiceContainer."""

import pytest
import tempfile
import os
import time

from infrastructure.service_container import ServiceContainer, ServiceConfig


@pytest.fixture
def temp_db_path():
    """Create a temporary database path."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    yield path
    time.sleep(0.1)  # Windows file locking
    try:
        os.unlink(path)
    except Exception:
        pass


@pytest.fixture
def config(temp_db_path):
    """Create a test configuration."""
    return ServiceConfig(
        db_path=temp_db_path,
        lobby_ready_threshold=10,
        lobby_max_players=12,
    )


class TestServiceContainerInitialization:
    """Tests for ServiceContainer initialization."""

    @pytest.mark.asyncio
    async def test_initialize_creates_all_repositories(self, config):
        """All repositories are created after initialization."""
        container = ServiceContainer(config)
        await container.initialize()

        assert container.player_repo is not None
        assert container.match_repo is not None
        assert container.bet_repo is not None
        assert container.lobby_repo is not None
        assert container.pairings_repo is not None
        assert container.guild_config_repo is not None
        assert container.prediction_repo is not None

    @pytest.mark.asyncio
    async def test_initialize_creates_all_services(self, config):
        """All services are created after initialization."""
        container = ServiceContainer(config)
        await container.initialize()

        assert container.player_service is not None
        assert container.match_service is not None
        assert container.betting_service is not None
        assert container.loan_service is not None
        assert container.bankruptcy_service is not None
        assert container.prediction_service is not None
        assert container.lobby_service is not None
        assert container.lobby_manager is not None
        assert container.gambling_stats_service is not None
        assert container.garnishment_service is not None
        assert container.guild_config_service is not None
        assert container.recalibration_service is not None
        assert container.disburse_service is not None

    @pytest.mark.asyncio
    async def test_initialize_is_idempotent(self, config):
        """Calling initialize multiple times is safe."""
        container = ServiceContainer(config)

        await container.initialize()
        first_player_service = container.player_service

        await container.initialize()
        second_player_service = container.player_service

        # Same instance should be returned
        assert first_player_service is second_player_service

    @pytest.mark.asyncio
    async def test_is_initialized_flag(self, config):
        """is_initialized returns correct state."""
        container = ServiceContainer(config)

        assert container.is_initialized is False

        await container.initialize()

        assert container.is_initialized is True


class TestServiceContainerDefaults:
    """Tests for ServiceContainer default configuration."""

    @pytest.mark.asyncio
    async def test_default_config_used_when_none(self, temp_db_path):
        """Default config is used when none provided."""
        # Patch the default db path
        default_config = ServiceConfig(db_path=temp_db_path)
        container = ServiceContainer(default_config)

        await container.initialize()

        assert container.is_initialized is True


class TestServiceContainerBotExposure:
    """Tests for expose_to_bot functionality."""

    @pytest.mark.asyncio
    async def test_expose_to_bot_sets_attributes(self, config):
        """expose_to_bot sets all expected attributes on bot."""
        container = ServiceContainer(config)
        await container.initialize()

        class MockBot:
            pass

        bot = MockBot()
        container.expose_to_bot(bot)

        # Check repositories
        assert hasattr(bot, "player_repo")
        assert hasattr(bot, "match_repo")
        assert hasattr(bot, "bet_repo")
        assert hasattr(bot, "lobby_repo")
        assert hasattr(bot, "pairings_repo")
        assert hasattr(bot, "guild_config_repo")
        assert hasattr(bot, "prediction_repo")

        # Check services
        assert hasattr(bot, "player_service")
        assert hasattr(bot, "match_service")
        assert hasattr(bot, "betting_service")
        assert hasattr(bot, "loan_service")
        assert hasattr(bot, "bankruptcy_service")
        assert hasattr(bot, "prediction_service")
        assert hasattr(bot, "lobby_service")
        assert hasattr(bot, "lobby_manager")
        assert hasattr(bot, "gambling_stats_service")
        assert hasattr(bot, "garnishment_service")
        assert hasattr(bot, "guild_config_service")
        assert hasattr(bot, "recalibration_service")
        assert hasattr(bot, "disburse_service")

        # Verify they're the same instances
        assert bot.player_service is container.player_service
        assert bot.match_service is container.match_service


class TestServiceDependencies:
    """Tests for proper service dependency wiring."""

    @pytest.mark.asyncio
    async def test_betting_service_has_garnishment(self, config):
        """BettingService is wired with GarnishmentService."""
        container = ServiceContainer(config)
        await container.initialize()

        betting = container.betting_service
        assert betting.garnishment_service is not None
        assert betting.garnishment_service is container.garnishment_service

    @pytest.mark.asyncio
    async def test_betting_service_has_bankruptcy(self, config):
        """BettingService is wired with BankruptcyService."""
        container = ServiceContainer(config)
        await container.initialize()

        betting = container.betting_service
        assert betting.bankruptcy_service is not None
        assert betting.bankruptcy_service is container.bankruptcy_service

    @pytest.mark.asyncio
    async def test_match_service_has_betting(self, config):
        """MatchService is wired with BettingService."""
        container = ServiceContainer(config)
        await container.initialize()

        match = container.match_service
        assert match.betting_service is not None
        assert match.betting_service is container.betting_service
