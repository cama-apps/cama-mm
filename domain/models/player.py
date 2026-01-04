"""
Player domain model.
"""

from dataclasses import dataclass


@dataclass
class Player:
    """
    Represents a player in the matchmaking system.

    This is a pure domain model with no infrastructure dependencies.
    """

    name: str
    mmr: int | None = None
    wins: int = 0
    losses: int = 0
    preferred_roles: list[str] | None = None  # e.g., ["1", "2", "3", "4", "5"]
    main_role: str | None = None
    # Glicko-2 rating fields
    glicko_rating: float | None = None
    glicko_rd: float | None = None
    glicko_volatility: float | None = None
    # Identity and economy
    discord_id: int | None = None
    jopacoin_balance: int = 0

    def get_value(self, use_glicko: bool = True) -> float:
        """
        Calculate player value for team balancing.

        Uses Glicko-2 rating if available, otherwise falls back to MMR.

        Args:
            use_glicko: Whether to use Glicko-2 rating (default True)

        Returns:
            Player value for balancing (in rating units)
        """
        if use_glicko and self.glicko_rating is not None:
            return self.glicko_rating

        return self.mmr if self.mmr is not None else 0

    def get_win_loss_differential(self) -> int:
        """Get wins minus losses."""
        return self.wins - self.losses

    def get_total_games(self) -> int:
        """Get total games played."""
        return self.wins + self.losses

    def get_win_rate(self) -> float | None:
        """Get win rate as a percentage, or None if no games played."""
        total = self.get_total_games()
        if total == 0:
            return None
        return (self.wins / total) * 100

    def has_role(self, role: str) -> bool:
        """Check if player has a specific preferred role."""
        if not self.preferred_roles:
            return False
        return role in self.preferred_roles

    def __str__(self) -> str:
        mmr_str = f"{self.mmr}" if self.mmr else "No MMR"
        return f"{self.name} (MMR: {mmr_str}, W-L: {self.wins}-{self.losses})"
