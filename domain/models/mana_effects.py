"""Data classes for the mana effects system.

Each of the 5 MTG colors has specific effects that modify economy behavior.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class ManaEffects:
    """Container for all mana color effects.

    Returned by the effects service to inform economy commands
    how a player's active mana color modifies their behavior.
    """

    # Identity
    color: str | None = None
    land: str | None = None

    # RED (Mountain) - High risk, high reward gambling
    red_10x_leverage: bool = False
    red_bomb_pot_ante: int = 10
    red_roll_cost: int = 1
    red_roll_jackpot: int = 20

    # BLUE (Island) - Information advantage with taxes
    blue_gamba_scrying: bool = False
    blue_gamba_reduction: float = 0.0
    blue_cashback_rate: float = 0.0
    blue_tax_rate: float = 0.0

    # GREEN (Forest) - Steady growth with caps
    green_steady_bonus: int = 0
    green_gain_cap: int | None = None
    green_bankrupt_penalty: int = -100
    green_max_wheel_win: int = 100

    # WHITE (Plains) - Protection and community
    plains_guardian_aura: bool = False
    plains_guardian_cooldown_key: str = "plains_guardian"
    plains_max_wheel_win: int | None = None
    plains_tip_fee_rate: float | None = None
    plains_tithe_rate: float = 0.0

    # BLACK (Swamp) - Parasitic with reduced penalties
    swamp_siphon: bool = False
    swamp_self_tax: int = 0
    swamp_bankruptcy_games: int = 5

    @classmethod
    def for_color(cls, color: str | None, land: str | None) -> ManaEffects:
        """Return a ManaEffects instance with values set for the given color.

        Args:
            color: One of "Red", "Blue", "Green", "White", "Black", or None.
            land: One of "Mountain", "Island", "Forest", "Plains", "Swamp", or None.

        Returns:
            ManaEffects with the appropriate effect values for the color.
            If color is None, returns defaults (no effects active).
        """
        if color is None:
            return cls()

        base = cls(color=color, land=land)

        if color == "Red":
            base.red_10x_leverage = True
            base.red_bomb_pot_ante = 30
            base.red_roll_cost = 2
            base.red_roll_jackpot = 40

        elif color == "Blue":
            base.blue_gamba_scrying = True
            base.blue_gamba_reduction = 0.25
            base.blue_cashback_rate = 0.05
            base.blue_tax_rate = 0.05

        elif color == "Green":
            base.green_steady_bonus = 1
            base.green_gain_cap = 50
            base.green_bankrupt_penalty = -50
            base.green_max_wheel_win = 60

        elif color == "White":
            base.plains_guardian_aura = True
            base.plains_max_wheel_win = 50
            base.plains_tip_fee_rate = 0.0
            base.plains_tithe_rate = 0.05

        elif color == "Black":
            base.swamp_siphon = True
            base.swamp_self_tax = 2
            base.swamp_bankruptcy_games = 3

        return base
