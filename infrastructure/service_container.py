"""
Service container for dependency injection and initialization.

This module centralizes service creation and wiring, replacing the
scattered initialization logic in bot.py.

Usage:
    container = ServiceContainer(config)
    await container.initialize()

    # Access services
    match_service = container.match_service
    betting_service = container.betting_service
"""

import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from services.bankruptcy_service import BankruptcyService
    from services.betting_service import BettingService
    from services.disburse_service import DisburseService
    from services.gambling_stats_service import GamblingStatsService
    from services.garnishment_service import GarnishmentService
    from services.guild_config_service import GuildConfigService
    from services.loan_service import LoanService
    from services.lobby_service import LobbyService
    from services.lobby_manager_service import LobbyManagerService
    from services.match_discovery_service import MatchDiscoveryService
    from services.match_enrichment_service import MatchEnrichmentService
    from services.match_service import MatchService
    from services.player_service import PlayerService
    from services.prediction_service import PredictionService
    from services.recalibration_service import RecalibrationService

from database import Database
from infrastructure.schema_manager import SchemaManager

# Repositories
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository
from repositories.bet_repository import BetRepository
from repositories.lobby_repository import LobbyRepository
from repositories.pairings_repository import PairingsRepository
from repositories.guild_config_repository import GuildConfigRepository
from repositories.prediction_repository import PredictionRepository
from repositories.disburse_repository import DisburseRepository

logger = logging.getLogger("cama_bot.infrastructure.container")


@dataclass
class RepositoryContainer:
    """Container for all repositories."""

    player: PlayerRepository | None = None
    match: MatchRepository | None = None
    bet: BetRepository | None = None
    lobby: LobbyRepository | None = None
    pairings: PairingsRepository | None = None
    guild_config: GuildConfigRepository | None = None
    prediction: PredictionRepository | None = None
    disburse: DisburseRepository | None = None


@dataclass
class ServiceConfig:
    """Configuration for service initialization."""

    # Database
    db_path: str = "cama_shuffle.db"

    # Lobby settings
    lobby_ready_threshold: int = 10
    lobby_max_players: int = 14

    # Rating settings
    off_role_multiplier: float = 0.95
    off_role_flat_penalty: float = 350.0

    # Economy settings
    max_debt: int = 500
    jopacoin_participation_reward: int = 1
    jopacoin_win_reward: int = 2
    jopacoin_exclusion_bonus: int = 1
    leverage_tiers: list[int] = field(default_factory=lambda: [2, 3, 5])

    # Loan settings
    loan_cooldown_seconds: int = 259200  # 3 days
    loan_max_amount: int = 100
    loan_fee_rate: float = 0.20

    # Bankruptcy settings
    bankruptcy_cooldown_seconds: int = 604800  # 7 days
    bankruptcy_penalty_games: int = 5
    bankruptcy_penalty_rate: float = 0.5

    # Optional features
    enable_ai_services: bool = False
    cerebras_api_key: str | None = None


class ServiceContainer:
    """
    Central container for all application services.

    Handles proper initialization order and dependency injection.
    Services are created lazily and cached.

    Example:
        container = ServiceContainer(config)
        await container.initialize()

        # Services are now available
        match_service = container.match_service
    """

    def __init__(self, config: ServiceConfig | None = None):
        """
        Initialize the container with configuration.

        Args:
            config: Service configuration (uses defaults if None)
        """
        self.config = config or ServiceConfig()
        self._initialized = False
        self._repos = RepositoryContainer()

        # Service instances (initialized lazily)
        self._database: Database | None = None
        self._services: dict[str, Any] = {}

    @property
    def is_initialized(self) -> bool:
        """Check if container has been initialized."""
        return self._initialized

    async def initialize(self) -> None:
        """
        Initialize all services in correct order.

        This method is idempotent - calling it multiple times has no effect.
        """
        if self._initialized:
            logger.debug("ServiceContainer already initialized, skipping")
            return

        logger.info("Initializing ServiceContainer...")

        # Initialize database and schema
        self._init_database()

        # Initialize repositories
        self._init_repositories()

        # Initialize services in dependency order
        self._init_core_services()
        self._init_economy_services()
        self._init_match_services()
        self._init_optional_services()

        # Wire post-construction dependencies
        self._wire_dependencies()

        self._initialized = True
        logger.info("ServiceContainer initialization complete")

    def _init_database(self) -> None:
        """Initialize database and run migrations."""
        logger.debug(f"Initializing database at {self.config.db_path}")

        schema_manager = SchemaManager(self.config.db_path)
        schema_manager.initialize()

        self._database = Database(self.config.db_path)

    def _init_repositories(self) -> None:
        """Initialize all repositories."""
        logger.debug("Initializing repositories")

        db_path = self.config.db_path
        self._repos.player = PlayerRepository(db_path)
        self._repos.match = MatchRepository(db_path)
        self._repos.bet = BetRepository(db_path)
        self._repos.lobby = LobbyRepository(db_path)
        self._repos.pairings = PairingsRepository(db_path)
        self._repos.guild_config = GuildConfigRepository(db_path)
        self._repos.prediction = PredictionRepository(db_path)
        self._repos.disburse = DisburseRepository(db_path)

    def _init_core_services(self) -> None:
        """Initialize core services with no complex dependencies."""
        logger.debug("Initializing core services")

        from services.guild_config_service import GuildConfigService
        from services.garnishment_service import GarnishmentService
        from services.bankruptcy_service import BankruptcyService, BankruptcyRepository
        from services.loan_service import LoanService, LoanRepository

        # Guild config
        self._services["guild_config"] = GuildConfigService(self._repos.guild_config)

        # Garnishment (debt repayment from winnings)
        self._services["garnishment"] = GarnishmentService(
            player_repo=self._repos.player,
        )

        # Bankruptcy
        bankruptcy_repo = BankruptcyRepository(self.config.db_path)
        self._services["bankruptcy"] = BankruptcyService(
            bankruptcy_repo=bankruptcy_repo,
            player_repo=self._repos.player,
            cooldown_seconds=self.config.bankruptcy_cooldown_seconds,
            penalty_games=self.config.bankruptcy_penalty_games,
            penalty_rate=self.config.bankruptcy_penalty_rate,
        )
        self._services["bankruptcy_repo"] = bankruptcy_repo

        # Loans
        loan_repo = LoanRepository(self.config.db_path)
        self._services["loan"] = LoanService(
            loan_repo=loan_repo,
            player_repo=self._repos.player,
            cooldown_seconds=self.config.loan_cooldown_seconds,
            max_amount=self.config.loan_max_amount,
            fee_rate=self.config.loan_fee_rate,
            max_debt=self.config.max_debt,
        )
        self._services["loan_repo"] = loan_repo

    def _init_economy_services(self) -> None:
        """Initialize economy/betting services."""
        logger.debug("Initializing economy services")

        from services.betting_service import BettingService
        from services.disburse_service import DisburseService
        from services.gambling_stats_service import GamblingStatsService
        from services.prediction_service import PredictionService
        from services.recalibration_service import RecalibrationService
        from repositories.recalibration_repository import RecalibrationRepository

        # Betting
        self._services["betting"] = BettingService(
            bet_repo=self._repos.bet,
            player_repo=self._repos.player,
            garnishment_service=self._services["garnishment"],
            bankruptcy_service=self._services["bankruptcy"],
            max_debt=self.config.max_debt,
            leverage_tiers=self.config.leverage_tiers,
        )

        # Disbursement
        self._services["disburse"] = DisburseService(
            disburse_repo=self._repos.disburse,
            loan_repo=self._services["loan_repo"],
            player_repo=self._repos.player,
        )

        # Gambling stats
        self._services["gambling_stats"] = GamblingStatsService(
            bet_repo=self._repos.bet,
            player_repo=self._repos.player,
            match_repo=self._repos.match,
            bankruptcy_service=self._services["bankruptcy"],
            loan_service=self._services["loan"],
        )

        # Predictions
        self._services["prediction"] = PredictionService(
            prediction_repo=self._repos.prediction,
            player_repo=self._repos.player,
        )

        # Recalibration
        recalibration_repo = RecalibrationRepository(self.config.db_path)
        self._services["recalibration"] = RecalibrationService(
            recalibration_repo=recalibration_repo,
            player_repo=self._repos.player,
        )

    def _init_match_services(self) -> None:
        """Initialize match-related services."""
        logger.debug("Initializing match services")

        from services.player_service import PlayerService
        from services.match_service import MatchService
        from services.lobby_service import LobbyService
        from services.lobby_manager_service import LobbyManagerService

        # Player service
        self._services["player"] = PlayerService(
            player_repo=self._repos.player,
        )

        # Lobby manager
        lobby_manager = LobbyManagerService(self._repos.lobby)
        self._services["lobby_manager"] = lobby_manager

        # Lobby service
        self._services["lobby"] = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=self._repos.player,
            bankruptcy_repo=self._services["bankruptcy_repo"],
            max_players=self.config.lobby_max_players,
            ready_threshold=self.config.lobby_ready_threshold,
        )

        # Match service
        self._services["match"] = MatchService(
            player_repo=self._repos.player,
            match_repo=self._repos.match,
            betting_service=self._services["betting"],
            pairings_repo=self._repos.pairings,
            loan_service=self._services["loan"],
        )

    def _init_optional_services(self) -> None:
        """Initialize optional services (enrichment, etc.)."""
        logger.debug("Initializing optional services")

        from services.match_enrichment_service import MatchEnrichmentService
        from services.match_discovery_service import MatchDiscoveryService

        # Match enrichment
        self._services["match_enrichment"] = MatchEnrichmentService(
            match_repo=self._repos.match,
            player_repo=self._repos.player,
        )

        # Match discovery
        self._services["match_discovery"] = MatchDiscoveryService(
            match_repo=self._repos.match,
            player_repo=self._repos.player,
        )

    def _wire_dependencies(self) -> None:
        """Wire any post-construction dependencies."""
        logger.debug("Wiring post-construction dependencies")

        # Example: match_service.stake_service would be set here
        # if stake service exists
        pass

    # =========================================================================
    # Service accessors
    # =========================================================================

    @property
    def player_repo(self) -> PlayerRepository:
        """Get player repository."""
        return self._repos.player

    @property
    def match_repo(self) -> MatchRepository:
        """Get match repository."""
        return self._repos.match

    @property
    def bet_repo(self) -> BetRepository:
        """Get bet repository."""
        return self._repos.bet

    @property
    def lobby_repo(self) -> LobbyRepository:
        """Get lobby repository."""
        return self._repos.lobby

    @property
    def pairings_repo(self) -> PairingsRepository:
        """Get pairings repository."""
        return self._repos.pairings

    @property
    def guild_config_repo(self) -> GuildConfigRepository:
        """Get guild config repository."""
        return self._repos.guild_config

    @property
    def prediction_repo(self) -> PredictionRepository:
        """Get prediction repository."""
        return self._repos.prediction

    @property
    def player_service(self) -> "PlayerService | None":
        """Get player service."""
        return self._services.get("player")

    @property
    def match_service(self) -> "MatchService | None":
        """Get match service."""
        return self._services.get("match")

    @property
    def betting_service(self) -> "BettingService | None":
        """Get betting service."""
        return self._services.get("betting")

    @property
    def loan_service(self) -> "LoanService | None":
        """Get loan service."""
        return self._services.get("loan")

    @property
    def bankruptcy_service(self) -> "BankruptcyService | None":
        """Get bankruptcy service."""
        return self._services.get("bankruptcy")

    @property
    def prediction_service(self) -> "PredictionService | None":
        """Get prediction service."""
        return self._services.get("prediction")

    @property
    def lobby_service(self) -> "LobbyService | None":
        """Get lobby service."""
        return self._services.get("lobby")

    @property
    def lobby_manager(self) -> "LobbyManagerService | None":
        """Get lobby manager service."""
        return self._services.get("lobby_manager")

    @property
    def gambling_stats_service(self) -> "GamblingStatsService | None":
        """Get gambling stats service."""
        return self._services.get("gambling_stats")

    @property
    def garnishment_service(self) -> "GarnishmentService | None":
        """Get garnishment service."""
        return self._services.get("garnishment")

    @property
    def guild_config_service(self) -> "GuildConfigService | None":
        """Get guild config service."""
        return self._services.get("guild_config")

    @property
    def recalibration_service(self) -> "RecalibrationService | None":
        """Get recalibration service."""
        return self._services.get("recalibration")

    @property
    def disburse_service(self) -> "DisburseService | None":
        """Get disburse service."""
        return self._services.get("disburse")

    @property
    def match_enrichment_service(self) -> "MatchEnrichmentService | None":
        """Get match enrichment service."""
        return self._services.get("match_enrichment")

    @property
    def match_discovery_service(self) -> "MatchDiscoveryService | None":
        """Get match discovery service."""
        return self._services.get("match_discovery")

    def expose_to_bot(self, bot) -> None:
        """
        Expose all services to a Discord bot object.

        This provides backward compatibility with existing code that
        accesses services via bot.<service_name>.

        Args:
            bot: The Discord bot instance
        """
        # Repositories
        bot.player_repo = self.player_repo
        bot.match_repo = self.match_repo
        bot.bet_repo = self.bet_repo
        bot.lobby_repo = self.lobby_repo
        bot.pairings_repo = self.pairings_repo
        bot.guild_config_repo = self.guild_config_repo
        bot.prediction_repo = self.prediction_repo

        # Services
        bot.player_service = self.player_service
        bot.match_service = self.match_service
        bot.betting_service = self.betting_service
        bot.loan_service = self.loan_service
        bot.bankruptcy_service = self.bankruptcy_service
        bot.prediction_service = self.prediction_service
        bot.lobby_service = self.lobby_service
        bot.lobby_manager = self.lobby_manager
        bot.gambling_stats_service = self.gambling_stats_service
        bot.garnishment_service = self.garnishment_service
        bot.guild_config_service = self.guild_config_service
        bot.recalibration_service = self.recalibration_service
        bot.disburse_service = self.disburse_service
        bot.match_enrichment_service = self.match_enrichment_service
        bot.match_discovery_service = self.match_discovery_service

        logger.info("Services exposed to bot object")
