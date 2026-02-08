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
    initial_mmr: int | None = None
    wins: int = 0
    losses: int = 0
    preferred_roles: list[str] | None = None  # e.g., ["1", "2", "3", "4", "5"]
    main_role: str | None = None
    # Glicko-2 rating fields
    glicko_rating: float | None = None
    glicko_rd: float | None = None
    glicko_volatility: float | None = None
    # OpenSkill Plackett-Luce rating fields (fantasy-weighted)
    os_mu: float | None = None
    os_sigma: float | None = None
    # Identity and economy
    discord_id: int | None = None
    guild_id: int | None = None  # Guild ID for multi-server isolation
    jopacoin_balance: int = 0
    steam_id: int | None = None  # Steam32 account ID for OpenDota integration
    # Easter egg tracking (JOPA-T expansion)
    personal_best_win_streak: int = 0
    total_bets_placed: int = 0
    first_leverage_used: bool = False

    def get_value(self, use_glicko: bool = True, use_openskill: bool = False, use_jopacoin: bool = False) -> float:
        """
        Calculate player value for team balancing.

        Priority order:
        1. Jopacoin balance (if use_jopacoin=True)
        2. OpenSkill (if use_openskill=True and available)
        3. Glicko-2 (if use_glicko=True and available)
        4. MMR fallback

        Args:
            use_glicko: Whether to use Glicko-2 rating (default True)
            use_openskill: Whether to use OpenSkill rating (default False)
            use_jopacoin: Whether to use jopacoin balance (default False)

        Returns:
            Player value for balancing (in rating units)
        """
        if use_jopacoin:
            return float(self.jopacoin_balance)

        if use_openskill:
            if self.os_mu is not None:
                # Convert OpenSkill mu to comparable scale with Glicko/MMR
                # Fantasy-weighted OpenSkill mu ~25-65 maps to ~0-3000
                # Formula matches CamaOpenSkillSystem.mu_to_display()
                return max(0, (self.os_mu - 25) * 75)
            # No os_mu stored - derive from Glicko or MMR using same scale
            # This ensures consistent balancing when some players lack OpenSkill data
            if self.glicko_rating is not None:
                return self.glicko_rating
            if self.mmr is not None:
                # MMR → Glicko scale (0-12000 → 0-3000)
                return self.mmr * 0.25
            return 0

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
