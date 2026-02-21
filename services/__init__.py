"""
Application services layer.

Services orchestrate business operations using repositories and domain services.
"""

from repositories.bankruptcy_repository import BankruptcyRepository
from services.bankruptcy_service import BankruptcyService
from services.betting_service import BettingService
from services.lobby_service import LobbyService
from services.match_service import MatchService
from services.permissions import has_admin_permission, has_allowlisted_admin
from services.player_service import PlayerService

# Result type for consistent error handling
from services.result import Result

# Service interfaces (ABCs)
from services.interfaces import (
    IPlayerService,
    IMatchService,
    IBettingService,
    ILoanService,
    IBankruptcyService,
    IPredictionService,
    ILobbyService,
    IGamblingStatsService,
    IGarnishmentService,
    IMatchEnrichmentService,
    IMatchDiscoveryService,
    IRecalibrationService,
    IDisburseService,
    IGuildConfigService,
)

__all__ = [
    # Concrete services
    "PlayerService",
    "MatchService",
    "LobbyService",
    "BettingService",
    "BankruptcyService",
    "BankruptcyRepository",
    # Permissions
    "has_admin_permission",
    "has_allowlisted_admin",
    # Result type
    "Result",
    # Interfaces
    "IPlayerService",
    "IMatchService",
    "IBettingService",
    "ILoanService",
    "IBankruptcyService",
    "IPredictionService",
    "ILobbyService",
    "IGamblingStatsService",
    "IGarnishmentService",
    "IMatchEnrichmentService",
    "IMatchDiscoveryService",
    "IRecalibrationService",
    "IDisburseService",
    "IGuildConfigService",
]
