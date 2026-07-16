"""Tests for ServiceContainer."""


from inspect import signature

from infrastructure.service_container import ServiceContainer


class TestServiceContainerInitialization:
    """Tests for ServiceContainer initialization."""

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

    def test_constructor_has_single_optional_llm_transport(self, repo_db_path):
        """The container accepts and stores only the supported LLM API key."""
        parameters = signature(ServiceContainer).parameters
        removed = {
            "loan_cooldown_seconds",
            "loan_max_amount",
            "loan_fee_rate",
            "cerebras_api_key",
        }

        assert {name for name in parameters if name.endswith("_api_key")} == {
            "llm_api_key"
        }
        assert parameters["llm_api_key"].default is None
        assert removed.isdisjoint(parameters)

        container = ServiceContainer(repo_db_path, llm_api_key="sentinel-key")
        assert container.llm_api_key == "sentinel-key"
        assert removed.isdisjoint(vars(container))

    def test_llm_api_key_is_passed_to_ai_service(self, repo_db_path, monkeypatch):
        """AIService receives the configured LLM API key unchanged."""
        import services.ai_service as ai_service_module

        captured = {}

        class CapturingAIService:
            def __init__(self, *, model, api_key, timeout, max_tokens):
                captured.update(
                    model=model,
                    api_key=api_key,
                    timeout=timeout,
                    max_tokens=max_tokens,
                )

        monkeypatch.setattr(ai_service_module, "AIService", CapturingAIService)

        container = ServiceContainer(repo_db_path, llm_api_key="sentinel-key")
        container.initialize()

        assert captured["api_key"] == "sentinel-key"
        assert isinstance(container._components["ai_service"], CapturingAIService)


class TestServiceContainerBotExposure:
    """Tests for expose_to_bot functionality."""

    def test_expose_to_bot_sets_supported_attributes_by_identity(self, repo_db_path):
        """expose_to_bot publishes only the supported component surface."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        class MockBot:
            pass

        bot = MockBot()
        container.expose_to_bot(bot)

        expected = {
            "player_repo": "player_repo",
            "match_repo": "match_repo",
            "bankruptcy_repo": "bankruptcy_repo",
            "player_service": "player_service",
            "match_service": "match_service",
            "betting_service": "betting_service",
            "loan_service": "loan_service",
            "bankruptcy_service": "bankruptcy_service",
            "prediction_service": "prediction_service",
            "lobby_service": "lobby_service",
            "lobby_manager": "lobby_manager",
            "gambling_stats_service": "gambling_stats_service",
            "balance_history_service": "balance_history_service",
            "guild_config_service": "guild_config_service",
            "recalibration_service": "recalibration_service",
            "disburse_service": "disburse_service",
            "tax_service": "tax_service",
            "match_enrichment_service": "match_enrichment_service",
            "match_discovery_service": "match_discovery_service",
            "pairings_service": "pairings_service",
            "soft_avoid_service": "soft_avoid_service",
            "package_deal_service": "package_deal_service",
            "tip_service": "tip_service",
            "opendota_player_service": "opendota_player_service",
            "rating_comparison_service": "rating_comparison_service",
            "neon_degen_service": "neon_degen_service",
            "wrapped_service": "wrapped_service",
            "mana_service": "mana_service",
            "mana_repo": "mana_repo",
            "mana_effects_service": "mana_effects_service",
            "protection_service": "protection_service",
            "buff_service": "buff_service",
            "dig_service": "dig_service",
            "dig_flavor_service": "dig_flavor_service",
            "reminder_service": "reminder_service",
            "activity_service": "activity_service",
            "curse_service": "curse_service",
            "mafia_service": "mafia_service",
            "mafia_flavor_service": "mafia_flavor_service",
            "sql_query_service": "sql_query_service",
            "flavor_text_service": "flavor_text_service",
        }
        removed = {
            "ADMIN_USER_IDS",
            "ai_service",
            "buff_repo",
            "curse_repo",
            "db",
            "dig_guild_modifier_repo",
            "dig_quest_repo",
            "dig_quest_service",
            "dig_repo",
            "economy_ledger_repo",
            "format_role_display",
            "guild_config_repo",
            "mafia_repo",
            "notification_repo",
            "package_deal_repo",
            "pairings_repo",
            "prediction_repo",
            "protection_repo",
            "role_emojis",
            "role_names",
            "slow_drip_repo",
            "soft_avoid_repo",
            "tax_repo",
            "tip_repository",
        }

        assert set(vars(bot)) == set(expected)
        for attribute, component in expected.items():
            assert getattr(bot, attribute) is container._components[component]
        for attribute in removed:
            assert not hasattr(bot, attribute)
        for attribute in (
            "dig_flavor_service",
            "sql_query_service",
            "flavor_text_service",
        ):
            assert getattr(bot, attribute) is None


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
