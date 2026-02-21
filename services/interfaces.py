"""
Service layer interfaces (ABCs).

These abstract base classes define the contracts for all services in the application.
Services should inherit from their corresponding interface to ensure consistent APIs.

Usage:
    class MyService(IMyService):
        def my_method(self, param: str) -> Result[dict]:
            ...

Benefits:
- Clear contracts for service methods
- Easier mocking in tests
- Documentation of expected behavior
- Type checking support
"""

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from domain.models.player import Player
    from domain.models.lobby import Lobby
    from services.result import Result
    from services.loan_service import (
        LoanState,
        LoanApproval,
        LoanResult,
        RepaymentResult,
    )
    from services.bankruptcy_service import (
        BankruptcyState,
        BankruptcyDeclaration,
        PenaltyApplication,
    )
    from services.gambling_stats_service import (
        GambaStats,
        DegenScoreBreakdown,
        Leaderboard,
        BettingImpactStats,
    )


class IPlayerService(ABC):
    """Interface for player registration and management."""

    @abstractmethod
    def register_player(
        self,
        discord_id: int,
        discord_username: str,
        steam_id: int,
        *,
        mmr_override: int | None = None,
    ) -> dict:
        """Register a new player with optional MMR override."""
        ...

    @abstractmethod
    def set_roles(self, discord_id: int, roles: list[str]) -> None:
        """Set preferred roles for a player."""
        ...

    @abstractmethod
    def get_player(self, discord_id: int) -> "Player | None":
        """Get a player by Discord ID."""
        ...

    @abstractmethod
    def get_balance(self, discord_id: int) -> int:
        """Get a player's jopacoin balance."""
        ...

    @abstractmethod
    def get_stats(self, discord_id: int) -> dict:
        """Get comprehensive stats for a player."""
        ...


class IMatchService(ABC):
    """Interface for match shuffling, recording, and voting."""

    @abstractmethod
    def get_last_shuffle(self, guild_id: int | None = None) -> dict | None:
        """Get the last shuffle state for a guild."""
        ...

    @abstractmethod
    def set_last_shuffle(self, guild_id: int | None, payload: dict) -> None:
        """Set the last shuffle state for a guild."""
        ...

    @abstractmethod
    def clear_last_shuffle(self, guild_id: int | None) -> None:
        """Clear the last shuffle state for a guild."""
        ...

    @abstractmethod
    def shuffle_players(
        self,
        player_ids: list[int],
        guild_id: int | None = None,
        betting_mode: str = "pool",
    ) -> dict:
        """Shuffle players into balanced teams."""
        ...

    @abstractmethod
    def record_match(
        self,
        winning_team: str,
        guild_id: int | None = None,
        dotabuff_match_id: str | None = None,
    ) -> dict:
        """Record a match result with rating updates."""
        ...

    @abstractmethod
    def add_record_submission(
        self,
        guild_id: int | None,
        user_id: int,
        result: str,
        is_admin: bool,
    ) -> dict[str, Any]:
        """Add a vote for recording a match result."""
        ...

    @abstractmethod
    def can_record_match(self, guild_id: int | None) -> bool:
        """Check if enough votes have been cast to record."""
        ...

    @abstractmethod
    def get_vote_counts(self, guild_id: int | None) -> dict[str, int]:
        """Get current vote counts for match recording."""
        ...

    @abstractmethod
    def add_abort_submission(
        self,
        guild_id: int | None,
        user_id: int,
        is_admin: bool,
    ) -> dict[str, Any]:
        """Add a vote to abort the current match."""
        ...

    @abstractmethod
    def can_abort_match(self, guild_id: int | None) -> bool:
        """Check if enough votes to abort the match."""
        ...


class IBettingService(ABC):
    """Interface for jopacoin betting on matches."""

    @abstractmethod
    def place_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        pending_state: dict[str, Any],
        leverage: int = 1,
    ) -> None:
        """Place a bet on a team (raises on error)."""
        ...

    @abstractmethod
    def settle_bets(
        self,
        match_id: int,
        guild_id: int | None,
        winning_team: str,
        pending_state: dict[str, Any],
    ) -> dict[str, list[dict]]:
        """Settle all bets for a completed match."""
        ...

    @abstractmethod
    def award_participation(
        self,
        player_ids: list[int],
    ) -> dict[int, dict[str, int]]:
        """Award participation jopacoin to players."""
        ...

    @abstractmethod
    def award_win_bonus(
        self,
        winning_ids: list[int],
    ) -> dict[int, dict[str, int]]:
        """Award win bonus jopacoin to winners."""
        ...

    @abstractmethod
    def get_pot_odds(
        self,
        guild_id: int | None,
        pending_state: dict[str, Any] | None = None,
    ) -> dict[str, int]:
        """Get current pot odds for pool betting."""
        ...

    @abstractmethod
    def get_pending_bets(
        self,
        guild_id: int | None,
        discord_id: int,
        pending_state: dict[str, Any] | None = None,
    ) -> list[dict]:
        """Get pending bets for a user."""
        ...

    @abstractmethod
    def refund_pending_bets(
        self,
        guild_id: int | None,
        pending_state: dict[str, Any] | None,
    ) -> int:
        """Refund all pending bets for an aborted match."""
        ...


class ILoanService(ABC):
    """Interface for jopacoin loan management."""

    @abstractmethod
    def get_state(self, discord_id: int) -> "LoanState":
        """Get the loan state for a player."""
        ...

    @abstractmethod
    def get_nonprofit_fund(self, guild_id: int | None) -> int:
        """Get the nonprofit fund balance."""
        ...

    # Result-returning methods (new API)

    @abstractmethod
    def validate_loan(self, discord_id: int, amount: int) -> "Result[LoanApproval]":
        """
        Check if a player can take a loan.

        Returns Result.ok(LoanApproval) if allowed, Result.fail otherwise.
        """
        ...

    @abstractmethod
    def execute_loan(
        self,
        discord_id: int,
        amount: int,
        guild_id: int | None = None,
    ) -> "Result[LoanResult]":
        """
        Take out a loan with deferred repayment.

        Returns Result.ok(LoanResult) on success.
        """
        ...

    @abstractmethod
    def execute_repayment(
        self,
        discord_id: int,
        guild_id: int | None = None,
    ) -> "Result[RepaymentResult]":
        """
        Repay an outstanding loan.

        Returns Result.ok(RepaymentResult) on success.
        """
        ...


class IBankruptcyService(ABC):
    """Interface for bankruptcy declaration and penalties."""

    @abstractmethod
    def get_state(self, discord_id: int) -> "BankruptcyState":
        """Get the bankruptcy state for a player."""
        ...

    @abstractmethod
    def get_bulk_states(
        self,
        discord_ids: list[int],
    ) -> dict[int, "BankruptcyState"]:
        """Get bankruptcy states for multiple players."""
        ...

    @abstractmethod
    def apply_penalty_to_winnings(
        self,
        discord_id: int,
        amount: int,
    ) -> dict[str, int]:
        """Apply bankruptcy penalty to winnings."""
        ...

    @abstractmethod
    def on_game_won(self, discord_id: int) -> int:
        """Decrement penalty games after winning (only wins count)."""
        ...

    # Result-returning methods (new API)

    @abstractmethod
    def validate_bankruptcy(self, discord_id: int) -> "Result[int]":
        """
        Check if a player can declare bankruptcy.

        Returns Result.ok(debt_amount) if allowed, Result.fail otherwise.
        """
        ...

    @abstractmethod
    def execute_bankruptcy(self, discord_id: int) -> "Result[BankruptcyDeclaration]":
        """
        Declare bankruptcy for a player.

        Returns Result.ok(BankruptcyDeclaration) on success.
        """
        ...

    @abstractmethod
    def calculate_penalized_winnings(
        self,
        discord_id: int,
        amount: int,
    ) -> "Result[PenaltyApplication]":
        """
        Calculate penalized winnings for a player under bankruptcy penalty.

        Returns Result.ok(PenaltyApplication) - always succeeds.
        """
        ...


class IPredictionService(ABC):
    """Interface for prediction market management."""

    @abstractmethod
    def create_prediction(
        self,
        guild_id: int,
        creator_id: int,
        question: str,
        closes_at: int,
        channel_id: int | None = None,
    ) -> dict[str, Any]:
        """Create a new prediction market."""
        ...

    @abstractmethod
    def place_bet(
        self,
        prediction_id: int,
        discord_id: int,
        position: str,
        amount: int,
    ) -> dict[str, Any]:
        """Place a bet on a prediction."""
        ...

    @abstractmethod
    def get_prediction(self, prediction_id: int) -> dict | None:
        """Get a prediction by ID."""
        ...

    @abstractmethod
    def get_active_predictions(self, guild_id: int) -> list[dict]:
        """Get all active predictions for a guild."""
        ...

    @abstractmethod
    def get_odds(self, prediction_id: int) -> dict[str, Any]:
        """Get current odds for a prediction."""
        ...

    @abstractmethod
    def add_resolution_vote(
        self,
        prediction_id: int,
        user_id: int,
        outcome: str,
        is_admin: bool | None = None,
    ) -> dict[str, Any]:
        """Add a vote to resolve a prediction."""
        ...

    @abstractmethod
    def can_resolve(
        self,
        prediction_id: int,
        outcome: str | None = None,
    ) -> bool:
        """Check if prediction can be resolved."""
        ...

    @abstractmethod
    def resolve(
        self,
        prediction_id: int,
        outcome: str,
        resolved_by: int,
    ) -> dict[str, Any]:
        """Resolve a prediction and pay out winners."""
        ...

    @abstractmethod
    def cancel(
        self,
        prediction_id: int,
        admin_id: int,
    ) -> dict[str, Any]:
        """Cancel a prediction and refund all bets."""
        ...


class ILobbyService(ABC):
    """Interface for lobby management."""

    @abstractmethod
    def get_or_create_lobby(self, creator_id: int | None = None) -> "Lobby":
        """Get existing lobby or create new one."""
        ...

    @abstractmethod
    def get_lobby(self) -> "Lobby | None":
        """Get current open lobby."""
        ...

    @abstractmethod
    def join_lobby(self, discord_id: int, guild_id: int = 0) -> tuple[bool, str, dict | None]:
        """Join the lobby. Returns (success, reason, pending_info)."""
        ...

    @abstractmethod
    def join_lobby_conditional(self, discord_id: int, guild_id: int = 0) -> tuple[bool, str, dict | None]:
        """Join conditional queue. Returns (success, reason, pending_info)."""
        ...

    @abstractmethod
    def leave_lobby(self, discord_id: int) -> bool:
        """Leave the lobby."""
        ...

    @abstractmethod
    def reset_lobby(self) -> None:
        """Reset the lobby."""
        ...

    @abstractmethod
    def set_lobby_message_id(
        self,
        message_id: int | None,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        origin_channel_id: int | None = None,
    ) -> None:
        """Set Discord message metadata for the lobby."""
        ...

    @abstractmethod
    def get_lobby_message_id(self) -> int | None:
        """Get the lobby message ID."""
        ...

    @abstractmethod
    def is_ready(self, lobby: "Lobby") -> bool:
        """Check if lobby has enough players to shuffle."""
        ...


class IGamblingStatsService(ABC):
    """Interface for gambling statistics and leaderboards."""

    @abstractmethod
    def get_player_stats(self, discord_id: int, guild_id: int | None = None) -> "GambaStats | None":
        """Get gambling stats for a player."""
        ...

    @abstractmethod
    def calculate_degen_score(self, discord_id: int, guild_id: int | None = None) -> "DegenScoreBreakdown":
        """Calculate degen score with breakdown."""
        ...

    @abstractmethod
    def get_leaderboard(
        self,
        guild_id: int | None,
        limit: int = 5,
        min_bets: int = 3,
    ) -> "Leaderboard":
        """Get gambling leaderboard."""
        ...

    @abstractmethod
    def get_betting_impact_stats(
        self,
        discord_id: int,
    ) -> "BettingImpactStats | None":
        """Get betting impact analysis for a player."""
        ...


class IGarnishmentService(ABC):
    """Interface for debt repayment from winnings."""

    @abstractmethod
    def garnish_winnings(
        self,
        discord_id: int,
        winnings: int,
    ) -> tuple[int, int]:
        """
        Garnish winnings for debt repayment.

        Returns:
            (amount_kept, amount_garnished)
        """
        ...


class IMatchEnrichmentService(ABC):
    """Interface for enriching matches with external data."""

    @abstractmethod
    def enrich_match(
        self,
        match_id: int,
        valve_match_id: int,
        source: str = "manual",
        confidence: float | None = None,
        skip_validation: bool = False,
        guild_id: int | None = None,
    ) -> dict:
        """Enrich a match with Valve API data."""
        ...


class IMatchDiscoveryService(ABC):
    """Interface for auto-discovering match IDs."""

    @abstractmethod
    def discover_matches_for_league(
        self,
        league_id: int,
        limit: int = 100,
    ) -> dict:
        """Discover match IDs for a league."""
        ...


class IRecalibrationService(ABC):
    """Interface for player rating recalibration."""

    @abstractmethod
    def can_recalibrate(self, discord_id: int, guild_id: int | None) -> dict:
        """Check if a player can recalibrate."""
        ...

    @abstractmethod
    def recalibrate(
        self,
        discord_id: int,
        guild_id: int | None,
        admin_id: int | None = None,
    ) -> dict:
        """Recalibrate a player's rating uncertainty."""
        ...


class IDisburseService(ABC):
    """Interface for nonprofit fund disbursement."""

    @abstractmethod
    def can_propose(self, guild_id: int) -> tuple[bool, str]:
        """Check if a disbursement can be proposed."""
        ...

    @abstractmethod
    def create_proposal(self, guild_id: int) -> Any:
        """Create a disbursement proposal."""
        ...

    @abstractmethod
    def add_vote(
        self,
        guild_id: int,
        user_id: int,
        method: str,
    ) -> dict:
        """Add a vote for disbursement method."""
        ...

    @abstractmethod
    def execute_disbursement(self, guild_id: int) -> dict:
        """Execute the winning disbursement method."""
        ...


class IGuildConfigService(ABC):
    """Interface for per-guild configuration."""

    @abstractmethod
    def get_league_id(self, guild_id: int) -> int | None:
        """Get the Valve league ID for a guild."""
        ...

    @abstractmethod
    def set_league_id(self, guild_id: int, league_id: int) -> None:
        """Set the Valve league ID for a guild."""
        ...

    @abstractmethod
    def get_auto_enrich(self, guild_id: int) -> bool:
        """Check if auto-enrichment is enabled."""
        ...

    @abstractmethod
    def set_auto_enrich(self, guild_id: int, enabled: bool) -> None:
        """Enable/disable auto-enrichment."""
        ...
