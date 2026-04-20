"""Data classes for the mana effects system.

Each of the 5 MTG colors has specific effects that modify economy behavior.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class ManaEffects:
    """Container for all mana color effects.

    Returned by the effects service to inform economy commands
    how a player's active mana color modifies their behavior.

    This is a pure value object — instances are immutable. Construct a new
    instance (e.g. via ``ManaEffects.for_color`` or ``dataclasses.replace``)
    rather than mutating attributes in place.
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

        if color == "Red":
            return cls(
                color=color,
                land=land,
                red_10x_leverage=True,
                red_bomb_pot_ante=30,
                red_roll_cost=2,
                red_roll_jackpot=40,
            )

        if color == "Blue":
            return cls(
                color=color,
                land=land,
                blue_gamba_scrying=True,
                blue_gamba_reduction=0.25,
                blue_cashback_rate=0.05,
                blue_tax_rate=0.05,
            )

        if color == "Green":
            return cls(
                color=color,
                land=land,
                green_steady_bonus=1,
                green_gain_cap=50,
                green_bankrupt_penalty=-50,
                green_max_wheel_win=60,
            )

        if color == "White":
            return cls(
                color=color,
                land=land,
                plains_guardian_aura=True,
                plains_max_wheel_win=50,
                plains_tip_fee_rate=0.0,
                plains_tithe_rate=0.05,
            )

        if color == "Black":
            return cls(
                color=color,
                land=land,
                swamp_siphon=True,
                swamp_self_tax=2,
                swamp_bankruptcy_games=3,
            )

        # Unknown color: return defaults with only identity set
        return cls(color=color, land=land)
