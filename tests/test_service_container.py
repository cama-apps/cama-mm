"""Tests for ServiceContainer."""

import pytest

from infrastructure.service_container import ServiceContainer


class TestServiceContainerInitialization:
    """Tests for ServiceContainer initialization."""

    def test_initialize_creates_all_repositories(self, repo_db_path):
        """All repositories are created after initialization."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        c = container._components
        assert c["player_repo"] is not None
        assert c["match_repo"] is not None
        assert c["bet_repo"] is not None
        assert c["lobby_repo"] is not None
        assert c["pairings_repo"] is not None
        assert c["guild_config_repo"] is not None
        assert c["prediction_repo"] is not None
        assert c["bankruptcy_repo"] is not None
        assert c["loan_repo"] is not None
        assert c["soft_avoid_repo"] is not None
        assert c["package_deal_repo"] is not None
        assert c["tip_repo"] is not None
        assert c["wrapped_repo"] is not None
        assert c["neon_event_repo"] is not None

    def test_initialize_creates_all_services(self, repo_db_path):
        """All services are created after initialization."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        c = container._components
        assert c["player_service"] is not None
        assert c["match_service"] is not None
        assert c["betting_service"] is not None
        assert c["loan_service"] is not None
        assert c["bankruptcy_service"] is not None
        assert c["prediction_service"] is not None
        assert c["lobby_service"] is not None
        assert c["lobby_manager"] is not None
        assert c["gambling_stats_service"] is not None
        assert c["garnishment_service"] is not None
        assert c["guild_config_service"] is not None
        assert c["recalibration_service"] is not None
        assert c["disburse_service"] is not None
        assert c["match_enrichment_service"] is not None
        assert c["match_discovery_service"] is not None
        assert c["rating_comparison_service"] is not None
        assert c["opendota_player_service"] is not None
        assert c["pairings_service"] is not None
        assert c["soft_avoid_service"] is not None
        assert c["package_deal_service"] is not None
        assert c["tip_service"] is not None
        assert c["match_state_service"] is not None
        assert c["neon_degen_service"] is not None
        assert c["wrapped_service"] is not None

    def test_initialize_is_idempotent(self, repo_db_path):
        """Calling initialize multiple times is safe."""
        container = ServiceContainer(repo_db_path)

        container.initialize()
        first = container._components["player_service"]

        container.initialize()
        second = container._components["player_service"]

        assert first is second

    def test_initialized_flag(self, repo_db_path):
        """_initialized returns correct state."""
        container = ServiceContainer(repo_db_path)
        assert container._initialized is False

        container.initialize()
        assert container._initialized is True


class TestServiceContainerBotExposure:
    """Tests for expose_to_bot functionality."""

    def test_expose_to_bot_sets_attributes(self, repo_db_path):
        """expose_to_bot sets all expected attributes on bot."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        class MockBot:
            pass

        bot = MockBot()
        container.expose_to_bot(bot)

        # Check repositories
        assert hasattr(bot, "player_repo")
        assert hasattr(bot, "match_repo")
        assert hasattr(bot, "pairings_repo")
        assert hasattr(bot, "guild_config_repo")
        assert hasattr(bot, "prediction_repo")
        assert hasattr(bot, "bankruptcy_repo")
        assert hasattr(bot, "soft_avoid_repo")
        assert hasattr(bot, "package_deal_repo")
        assert hasattr(bot, "tip_repository")

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
        assert hasattr(bot, "guild_config_service")
        assert hasattr(bot, "recalibration_service")
        assert hasattr(bot, "disburse_service")
        assert hasattr(bot, "match_enrichment_service")
        assert hasattr(bot, "match_discovery_service")
        assert hasattr(bot, "rating_comparison_service")
        assert hasattr(bot, "opendota_player_service")
        assert hasattr(bot, "pairings_service")
        assert hasattr(bot, "soft_avoid_service")
        assert hasattr(bot, "package_deal_service")
        assert hasattr(bot, "tip_service")
        assert hasattr(bot, "neon_degen_service")
        assert hasattr(bot, "wrapped_service")

        # Check constants
        assert hasattr(bot, "role_emojis")
        assert hasattr(bot, "role_names")
        assert hasattr(bot, "format_role_display")
        assert hasattr(bot, "ADMIN_USER_IDS")

        # AI services default to None without key
        assert bot.ai_service is None
        assert bot.sql_query_service is None
        assert bot.flavor_text_service is None


class TestServiceDependencies:
    """Tests for proper service dependency wiring."""

    def test_betting_service_has_garnishment(self, repo_db_path):
        """BettingService is wired with GarnishmentService."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        betting = container._components["betting_service"]
        garnishment = container._components["garnishment_service"]
        assert betting.garnishment_service is not None
        assert betting.garnishment_service is garnishment

    def test_betting_service_has_bankruptcy(self, repo_db_path):
        """BettingService is wired with BankruptcyService."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        betting = container._components["betting_service"]
        bankruptcy = container._components["bankruptcy_service"]
        assert betting.bankruptcy_service is not None
        assert betting.bankruptcy_service is bankruptcy

    def test_match_service_has_betting(self, repo_db_path):
        """MatchService is wired with BettingService."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        match = container._components["match_service"]
        betting = container._components["betting_service"]
        assert match.betting_service is not None
        assert match.betting_service is betting

    def test_match_service_has_state_service(self, repo_db_path):
        """MatchService is wired with MatchStateService."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        match = container._components["match_service"]
        state = container._components["match_state_service"]
        assert match.state_service is not None
        assert match.state_service is state

    def test_lobby_service_has_state_service(self, repo_db_path):
        """LobbyService is wired with MatchStateService."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        lobby = container._components["lobby_service"]
        state = container._components["match_state_service"]
        assert lobby.match_state_service is not None
        assert lobby.match_state_service is state
