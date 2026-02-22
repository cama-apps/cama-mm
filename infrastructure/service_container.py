"""
Service container for dependency injection and initialization.

Centralizes all service creation and wiring, replacing the
scattered initialization logic in bot.py.

Usage:
    container = ServiceContainer(db_path, admin_user_ids=ADMIN_USER_IDS)
    container.initialize()
    container.expose_to_bot(bot)
"""

import logging

logger = logging.getLogger("cama_bot.infrastructure.container")


class ServiceContainer:
    """
    Central container for all application services.

    Creates all repositories and services in the correct dependency
    order, then exposes them to the bot via expose_to_bot().
    """

    def __init__(
        self,
        db_path: str,
        *,
        # Admin
        admin_user_ids: list[int] | None = None,
        # Lobby
        lobby_ready_threshold: int = 10,
        lobby_max_players: int = 14,
        # Rating
        use_glicko: bool = True,
        # Economy
        max_debt: int = 500,
        leverage_tiers: list[int] | None = None,
        garnishment_percentage: float = 1.0,
        # Loan
        loan_cooldown_seconds: int = 259200,
        loan_max_amount: int = 100,
        loan_fee_rate: float = 0.20,
        # AI (optional)
        cerebras_api_key: str | None = None,
        ai_model: str = "cerebras/llama-4-scout-17b-16e-instruct",
        ai_timeout_seconds: int = 30,
        ai_max_tokens: int = 4096,
    ):
        self.db_path = db_path
        self.admin_user_ids = admin_user_ids or []
        self.lobby_ready_threshold = lobby_ready_threshold
        self.lobby_max_players = lobby_max_players
        self.use_glicko = use_glicko
        self.max_debt = max_debt
        self.leverage_tiers = leverage_tiers or [2, 3, 5]
        self.garnishment_percentage = garnishment_percentage
        self.loan_cooldown_seconds = loan_cooldown_seconds
        self.loan_max_amount = loan_max_amount
        self.loan_fee_rate = loan_fee_rate
        self.cerebras_api_key = cerebras_api_key
        self.ai_model = ai_model
        self.ai_timeout_seconds = ai_timeout_seconds
        self.ai_max_tokens = ai_max_tokens

        self._initialized = False

        # All repos/services stored by name
        self._components: dict = {}

    def initialize(self) -> None:
        """Initialize all repositories and services in dependency order."""
        if self._initialized:
            return

        logger.info("Initializing ServiceContainer...")

        self._init_database()
        self._init_repositories()
        self._init_core_services()
        self._init_economy_services()
        self._init_match_services()
        self._init_advanced_services()
        self._init_ai_services()
        self._init_extras()

        self._initialized = True
        logger.info("ServiceContainer initialization complete")

    # ------------------------------------------------------------------
    # Initialization stages
    # ------------------------------------------------------------------

    def _init_database(self) -> None:
        from database import Database

        self._components["db"] = Database(db_path=self.db_path)

    def _init_repositories(self) -> None:
        from repositories.player_repository import PlayerRepository
        from repositories.match_repository import MatchRepository
        from repositories.bet_repository import BetRepository
        from repositories.lobby_repository import LobbyRepository
        from repositories.pairings_repository import PairingsRepository
        from repositories.guild_config_repository import GuildConfigRepository
        from repositories.prediction_repository import PredictionRepository
        from repositories.disburse_repository import DisburseRepository
        from repositories.bankruptcy_repository import BankruptcyRepository
        from repositories.loan_repository import LoanRepository
        from repositories.recalibration_repository import RecalibrationRepository
        from repositories.soft_avoid_repository import SoftAvoidRepository
        from repositories.package_deal_repository import PackageDealRepository
        from repositories.tip_repository import TipRepository
        from repositories.neon_event_repository import NeonEventRepository
        from repositories.wrapped_repository import WrappedRepository

        p = self.db_path
        self._components.update({
            "player_repo": PlayerRepository(p),
            "match_repo": MatchRepository(p),
            "bet_repo": BetRepository(p),
            "lobby_repo": LobbyRepository(p),
            "pairings_repo": PairingsRepository(p),
            "guild_config_repo": GuildConfigRepository(p),
            "prediction_repo": PredictionRepository(p),
            "disburse_repo": DisburseRepository(p),
            "bankruptcy_repo": BankruptcyRepository(p),
            "loan_repo": LoanRepository(p),
            "recalibration_repo": RecalibrationRepository(p),
            "soft_avoid_repo": SoftAvoidRepository(p),
            "package_deal_repo": PackageDealRepository(p),
            "tip_repo": TipRepository(p),
            "neon_event_repo": NeonEventRepository(p),
            "wrapped_repo": WrappedRepository(p),
        })

    def _init_core_services(self) -> None:
        """Services with no dependencies on other services."""
        from services.guild_config_service import GuildConfigService
        from services.garnishment_service import GarnishmentService
        from services.bankruptcy_service import BankruptcyService
        from services.loan_service import LoanService
        from services.recalibration_service import RecalibrationService
        from services.pairings_service import PairingsService
        from services.soft_avoid_service import SoftAvoidService
        from services.package_deal_service import PackageDealService
        from services.tip_service import TipService
        from services.opendota_player_service import OpenDotaPlayerService
        from services.match_state_service import MatchStateService

        c = self._components
        c["guild_config_service"] = GuildConfigService(c["guild_config_repo"])
        c["garnishment_service"] = GarnishmentService(
            c["player_repo"], self.garnishment_percentage
        )
        c["bankruptcy_service"] = BankruptcyService(
            c["bankruptcy_repo"], c["player_repo"]
        )
        c["loan_service"] = LoanService(c["loan_repo"], c["player_repo"])
        c["recalibration_service"] = RecalibrationService(
            c["recalibration_repo"], c["player_repo"]
        )
        c["pairings_service"] = PairingsService(c["pairings_repo"])
        c["soft_avoid_service"] = SoftAvoidService(c["soft_avoid_repo"])
        c["package_deal_service"] = PackageDealService(c["package_deal_repo"])
        c["tip_service"] = TipService(c["tip_repo"])
        c["opendota_player_service"] = OpenDotaPlayerService(c["player_repo"])
        c["match_state_service"] = MatchStateService(c["match_repo"])

    def _init_economy_services(self) -> None:
        """Services that depend on core services."""
        from services.betting_service import BettingService
        from services.disburse_service import DisburseService
        from services.gambling_stats_service import GamblingStatsService
        from services.prediction_service import PredictionService

        c = self._components
        c["betting_service"] = BettingService(
            bet_repo=c["bet_repo"],
            player_repo=c["player_repo"],
            garnishment_service=c["garnishment_service"],
            bankruptcy_service=c["bankruptcy_service"],
            max_debt=self.max_debt,
            leverage_tiers=self.leverage_tiers,
        )
        c["disburse_service"] = DisburseService(
            c["disburse_repo"], c["player_repo"], c["loan_repo"]
        )
        c["gambling_stats_service"] = GamblingStatsService(
            bet_repo=c["bet_repo"],
            player_repo=c["player_repo"],
            match_repo=c["match_repo"],
            bankruptcy_service=c["bankruptcy_service"],
            loan_service=c["loan_service"],
            loan_repo=c["loan_repo"],
        )
        c["prediction_service"] = PredictionService(
            prediction_repo=c["prediction_repo"],
            player_repo=c["player_repo"],
            admin_user_ids=self.admin_user_ids,
        )

    def _init_match_services(self) -> None:
        """Player, lobby, and match services."""
        from services.player_service import PlayerService
        from services.lobby_manager_service import LobbyManagerService
        from services.lobby_service import LobbyService
        from services.match_service import MatchService

        c = self._components
        c["player_service"] = PlayerService(c["player_repo"])
        c["lobby_manager"] = LobbyManagerService(c["lobby_repo"])
        c["lobby_service"] = LobbyService(
            lobby_manager=c["lobby_manager"],
            player_repo=c["player_repo"],
            ready_threshold=self.lobby_ready_threshold,
            max_players=self.lobby_max_players,
            bankruptcy_repo=c["bankruptcy_repo"],
            match_state_service=c["match_state_service"],
        )
        c["match_service"] = MatchService(
            player_repo=c["player_repo"],
            match_repo=c["match_repo"],
            use_glicko=self.use_glicko,
            betting_service=c["betting_service"],
            pairings_repo=c["pairings_repo"],
            loan_service=c["loan_service"],
            soft_avoid_repo=c["soft_avoid_repo"],
            package_deal_repo=c["package_deal_repo"],
            state_service=c["match_state_service"],
        )

    def _init_advanced_services(self) -> None:
        """Services that depend on match_service."""
        from services.rating_comparison_service import RatingComparisonService
        from services.match_enrichment_service import MatchEnrichmentService
        from services.match_discovery_service import MatchDiscoveryService

        c = self._components
        c["rating_comparison_service"] = RatingComparisonService(
            match_repo=c["match_repo"],
            player_repo=c["player_repo"],
            match_service=c["match_service"],
        )
        c["match_enrichment_service"] = MatchEnrichmentService(
            match_repo=c["match_repo"],
            player_repo=c["player_repo"],
            match_service=c["match_service"],
        )
        c["match_discovery_service"] = MatchDiscoveryService(
            match_repo=c["match_repo"],
            player_repo=c["player_repo"],
            match_service=c["match_service"],
        )

    def _init_ai_services(self) -> None:
        """Optional AI services (only if cerebras_api_key is set)."""
        c = self._components
        ai_service = None
        sql_query_service = None
        flavor_text_service = None

        if self.cerebras_api_key:
            try:
                from services.ai_service import AIService
                from services.sql_query_service import SQLQueryService
                from services.flavor_text_service import FlavorTextService
                from repositories.ai_query_repository import AIQueryRepository

                ai_service = AIService(
                    model=self.ai_model,
                    api_key=self.cerebras_api_key,
                    timeout=self.ai_timeout_seconds,
                    max_tokens=self.ai_max_tokens,
                )
                ai_query_repo = AIQueryRepository(self.db_path)
                sql_query_service = SQLQueryService(
                    ai_service=ai_service,
                    ai_query_repo=ai_query_repo,
                    guild_config_repo=c["guild_config_repo"],
                )
                flavor_text_service = FlavorTextService(
                    ai_service=ai_service,
                    player_repo=c["player_repo"],
                    bankruptcy_service=c["bankruptcy_service"],
                    loan_service=c["loan_service"],
                    gambling_stats_service=c["gambling_stats_service"],
                    guild_config_repo=c["guild_config_repo"],
                )
                logger.info(f"AI services initialized with model: {self.ai_model}")
            except Exception as e:
                logger.warning(f"Failed to initialize AI services: {e}")
                ai_service = None
                sql_query_service = None
                flavor_text_service = None
        else:
            logger.info("AI services not initialized (no API key)")

        c["ai_service"] = ai_service
        c["sql_query_service"] = sql_query_service
        c["flavor_text_service"] = flavor_text_service

    def _init_extras(self) -> None:
        """Neon Degen Terminal and Wrapped services."""
        from services.neon_degen_service import NeonDegenService
        from services.wrapped_service import WrappedService

        c = self._components
        c["neon_degen_service"] = NeonDegenService(
            player_repo=c["player_repo"],
            bet_repo=c["bet_repo"],
            bankruptcy_service=c["bankruptcy_service"],
            gambling_stats_service=c["gambling_stats_service"],
            ai_service=c["ai_service"],
            flavor_text_service=c["flavor_text_service"],
            neon_event_repo=c["neon_event_repo"],
        )
        c["wrapped_service"] = WrappedService(
            wrapped_repo=c["wrapped_repo"],
            player_repo=c["player_repo"],
            match_repo=c["match_repo"],
            bet_repo=c["bet_repo"],
            gambling_stats_service=c["gambling_stats_service"],
        )

    # ------------------------------------------------------------------
    # Bot exposure
    # ------------------------------------------------------------------

    def expose_to_bot(self, bot) -> None:
        """
        Attach all services and repositories to the bot object.

        This provides backward compatibility with existing cog code
        that accesses services via bot.<service_name>.
        """
        from utils.formatting import ROLE_EMOJIS, ROLE_NAMES, format_role_display

        c = self._components

        # Database
        bot.db = c["db"]

        # Repositories accessed directly by cogs
        bot.player_repo = c["player_repo"]
        bot.match_repo = c["match_repo"]
        bot.pairings_repo = c["pairings_repo"]
        bot.guild_config_repo = c["guild_config_repo"]
        bot.prediction_repo = c["prediction_repo"]
        bot.bankruptcy_repo = c["bankruptcy_repo"]
        bot.soft_avoid_repo = c["soft_avoid_repo"]
        bot.package_deal_repo = c["package_deal_repo"]
        bot.tip_repository = c["tip_repo"]

        # Services
        bot.player_service = c["player_service"]
        bot.match_service = c["match_service"]
        bot.betting_service = c["betting_service"]
        bot.loan_service = c["loan_service"]
        bot.bankruptcy_service = c["bankruptcy_service"]
        bot.prediction_service = c["prediction_service"]
        bot.lobby_service = c["lobby_service"]
        bot.lobby_manager = c["lobby_manager"]
        bot.gambling_stats_service = c["gambling_stats_service"]
        bot.guild_config_service = c["guild_config_service"]
        bot.recalibration_service = c["recalibration_service"]
        bot.disburse_service = c["disburse_service"]
        bot.match_enrichment_service = c["match_enrichment_service"]
        bot.match_discovery_service = c["match_discovery_service"]
        bot.pairings_service = c["pairings_service"]
        bot.soft_avoid_service = c["soft_avoid_service"]
        bot.package_deal_service = c["package_deal_service"]
        bot.tip_service = c["tip_service"]
        bot.opendota_player_service = c["opendota_player_service"]
        bot.rating_comparison_service = c["rating_comparison_service"]
        bot.neon_degen_service = c["neon_degen_service"]
        bot.wrapped_service = c["wrapped_service"]

        # AI services (may be None)
        bot.ai_service = c["ai_service"]
        bot.sql_query_service = c["sql_query_service"]
        bot.flavor_text_service = c["flavor_text_service"]

        # Constants and helpers
        bot.role_emojis = ROLE_EMOJIS
        bot.role_names = ROLE_NAMES
        bot.format_role_display = format_role_display
        bot.ADMIN_USER_IDS = self.admin_user_ids

        logger.info("Services exposed to bot object")
