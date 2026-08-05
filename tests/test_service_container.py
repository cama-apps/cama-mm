"""Tests for ServiceContainer."""


import time
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

    def test_duel_repository_and_service_are_initialized_once(self, repo_db_path):
        """Duel components share one stable repository instance."""
        container = ServiceContainer(repo_db_path)

        container.initialize()
        repo = container._components["duel_repo"]
        service = container._components["duel_service"]
        container.initialize()

        assert container._components["duel_repo"] is repo
        assert container._components["duel_service"] is service
        assert service.repo is repo

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

    def test_economy_controller_has_no_static_recovery_switch(self, repo_db_path):
        parameters = signature(ServiceContainer).parameters

        assert "economy_recovery_mode" not in parameters

        container = ServiceContainer(repo_db_path, economy_events_enabled=True)
        container.initialize()

        assert container._components["disburse_service"].voting_enabled is True
        assert (
            container._components["economy_event_service"]
            .ensure_policy(123)["mode"]
            == "normal"
        )

    def test_severe_persisted_edict_dynamically_disables_reserve_voting(
        self,
        repo_db_path,
    ):
        container = ServiceContainer(repo_db_path, economy_events_enabled=True)
        container.initialize()
        economy = container._components["economy_event_service"]
        now = int(time.time())
        event_date = economy._event_date_for_timestamp(now)
        container._components["economy_event_repo"].activate_event_atomic(
            123,
            {
                "event_date": event_date,
                "name": "Ravage",
                "hero": "Tidehunter",
                "direction": "deflationary",
                "severity": 3,
                "target_effect_jc": 0,
                "forecast_flow_jc": 0,
                "expected_effect_jc": 0,
                "monetary_stock_before": 0,
                "effects": {"reserve_voting_disabled": True},
                "announcement": "A tidal shock hits the economy.",
                "starts_at": now - 1,
                "ends_at": now + 60,
                "created_at": now,
            },
        )

        assert container._components["disburse_service"].can_propose(123) == (
            False,
            "economy_edict",
        )

    def test_llm_api_key_is_passed_to_ai_service(self, repo_db_path, monkeypatch):
        """AIService receives the configured LLM API key unchanged."""
        import services.ai_service as ai_service_module

        captured = {}

        class CapturingAIService:
            def __init__(
                self,
                *,
                model,
                api_key,
                timeout,
                max_tokens,
                request_repo,
            ):
                captured.update(
                    model=model,
                    api_key=api_key,
                    timeout=timeout,
                    max_tokens=max_tokens,
                    request_repo=request_repo,
                )

        monkeypatch.setattr(ai_service_module, "AIService", CapturingAIService)

        container = ServiceContainer(repo_db_path, llm_api_key="sentinel-key")
        container.initialize()

        assert captured["api_key"] == "sentinel-key"
        assert captured["request_repo"] is container._components["llm_request_repo"]
        assert isinstance(container._components["ai_service"], CapturingAIService)

    def test_dig_llm_hard_switch_preserves_other_ai_services(
        self,
        repo_db_path,
    ):
        """The Dig kill switch suppresses Dig LLM wiring without disabling /ask."""
        container = ServiceContainer(
            repo_db_path,
            llm_api_key="sentinel-key",
            dig_llm_enabled=False,
        )

        container.initialize()

        assert container._components["ai_service"] is not None
        assert container._components["sql_query_service"] is not None
        assert container._components["dig_flavor_service"] is None
        assert container._components["neon_degen_service"].dig_llm_enabled is False


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
            "low_priority_repo": "low_priority_repo",
            "player_service": "player_service",
            "match_service": "match_service",
            "betting_service": "betting_service",
            "loan_service": "loan_service",
            "bankruptcy_service": "bankruptcy_service",
            "vanity_tax_service": "vanity_tax_service",
            "prediction_service": "prediction_service",
            "lobby_service": "lobby_service",
            "lobby_manager": "lobby_manager",
            "gambling_stats_service": "gambling_stats_service",
            "balance_history_service": "balance_history_service",
            "guild_config_service": "guild_config_service",
            "recalibration_service": "recalibration_service",
            "moderation_service": "moderation_service",
            "disburse_service": "disburse_service",
            "tax_service": "tax_service",
            "economy_event_service": "economy_event_service",
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
            "duel_service": "duel_service",
            "duel_flavor_service": "duel_flavor_service",
            "reminder_service": "reminder_service",
            "mafia_service": "mafia_service",
            "mafia_flavor_service": "mafia_flavor_service",
            "pet_service": "pet_service",
            "pet_evolution_service": "pet_evolution_service",
            "pet_flavor_service": "pet_flavor_service",
            "pet_brawl_service": "pet_brawl_service",
            "player_trivia_service": "player_trivia_service",
            "sql_query_service": "sql_query_service",
            "flavor_text_service": "flavor_text_service",
        }
        removed = {
            "ADMIN_USER_IDS",
            "ai_service",
            "buff_repo",
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

    def test_duel_flavor_service_is_always_exposed(self, repo_db_path):
        """Static duel narration remains available without an AI provider."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        class MockBot:
            pass

        bot = MockBot()
        container.expose_to_bot(bot)

        assert bot.duel_flavor_service is container._components["duel_flavor_service"]
        assert bot.duel_flavor_service.ai_service is None


class TestServiceDependencies:
    """Tests for proper service dependency wiring."""

    def test_duel_flavor_service_has_optional_ai_and_guild_config(
        self, repo_db_path
    ):
        """Duel flavor receives the shared optional AI and guild config services."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        flavor = container._components["duel_flavor_service"]

        assert flavor.ai_service is container._components["ai_service"]
        assert flavor.guild_config_repo is container._components["guild_config_repo"]

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

    def test_dig_service_receives_pet_service_via_constructor(
        self, repo_db_path, monkeypatch
    ):
        """DigService is wired with PetService at construction time."""
        import config

        monkeypatch.setattr(config, "PET_CHANNEL_ID", 12345)
        container = ServiceContainer(repo_db_path)
        container.initialize()

        pet = container._components["pet_service"]
        assert pet is not None
        assert container._components["dig_service"].pet_service is pet

    def test_lobby_service_has_state_service(self, repo_db_path):
        """LobbyService is wired with MatchStateService."""
        container = ServiceContainer(repo_db_path)
        container.initialize()

        lobby = container._components["lobby_service"]
        state = container._components["match_state_service"]
        assert lobby.match_state_service is not None
        assert lobby.match_state_service is state
