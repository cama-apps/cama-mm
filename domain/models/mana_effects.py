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

    # --- Dig-unique mechanics (silent) ---
    dig_yield_variance: float = 0.0  # Mountain: probability of double/zero per block
    dig_hazard_modifier: float = 0.0  # Forest: -, Mountain/Black: + (percentage points)
    dig_durability_bonus_ticks: int = 0  # Forest: +1 hard hat tick today
    dig_dynamite_overyield_chance: float = 0.0  # Mountain: chance of +1 block
    dig_paid_cost_modifier_pct: float = 0.0  # Mountain: -5% paid-dig cost
    dig_cooldown_reduction_seconds: int = 0  # Forest: shave next-dig cooldown
    dig_paid_refund_on_caveins: float = 0.0  # Blue: refund fraction of paid-dig on cave-in

    # --- Boss combat unique mechanics (silent) ---
    boss_damage_variance_modifier: float = 0.0  # Mountain: + variance, Forest: - variance
    boss_durability_refund_rate: float = 0.0  # Blue: refund a fraction of durability lost
    boss_durability_prevention_rate: float = 0.0  # White: prevent a fraction of durability loss

    # --- Match betting (extending beyond red leverage) ---
    match_bet_steady_bonus: int = 0  # Green: +1 JC per match bet placed

    # --- Predictions ---
    pred_fee_discount_rate: float = 0.0  # Blue: -5% trade fee
    pred_steady_bonus: int = 0  # Green: +1 JC on profitable close

    # --- Shop discounts ---
    shop_info_discount_rate: float = 0.0  # Blue: -10% on info-style items
    shop_consumable_discount_rate: float = 0.0  # Green: -5% on consumables

    # --- Trivia ---
    trivia_streak_bonus: int = 0  # Green: +1 JC per streak milestone
    trivia_payout_multiplier: float = 1.0  # Red: +50% milestone payout
    trivia_hint_bonus: int = 0  # Blue: +1 hint slot per session

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
                # Dig: variance bump on yields + hazards
                dig_yield_variance=0.15,
                dig_hazard_modifier=0.01,
                dig_dynamite_overyield_chance=0.10,
                dig_paid_cost_modifier_pct=-0.05,
                # Boss combat: damage variance bump
                boss_damage_variance_modifier=0.15,
                # Trivia: amplified milestone payouts
                trivia_payout_multiplier=1.5,
            )

        if color == "Blue":
            return cls(
                color=color,
                land=land,
                blue_gamba_scrying=True,
                blue_gamba_reduction=0.25,
                blue_cashback_rate=0.05,
                blue_tax_rate=0.05,
                # Dig: paid refund on cave-in (existing tax/cashback fields apply too)
                dig_paid_refund_on_caveins=0.5,
                # Boss combat: durability refund
                boss_durability_refund_rate=0.25,
                # Predictions: trade fee discount
                pred_fee_discount_rate=0.05,
                # Shop: info-item discount
                shop_info_discount_rate=0.10,
                # Trivia: bonus hint
                trivia_hint_bonus=1,
            )

        if color == "Green":
            return cls(
                color=color,
                land=land,
                green_steady_bonus=1,
                green_gain_cap=50,
                green_bankrupt_penalty=-50,
                green_max_wheel_win=60,
                # Dig: -hazard, +durability tick, cooldown reduction
                dig_hazard_modifier=-0.01,
                dig_durability_bonus_ticks=1,
                dig_cooldown_reduction_seconds=30,
                # Boss combat: variance narrowing
                boss_damage_variance_modifier=-0.10,
                # Match betting: per-bet steady bonus
                match_bet_steady_bonus=1,
                # Predictions: steady bonus on profitable close
                pred_steady_bonus=1,
                # Shop: consumable discount
                shop_consumable_discount_rate=0.05,
                # Trivia: per-milestone steady bonus
                trivia_streak_bonus=1,
            )

        if color == "White":
            return cls(
                color=color,
                land=land,
                plains_guardian_aura=True,
                plains_max_wheel_win=50,
                plains_tip_fee_rate=0.0,
                plains_tithe_rate=0.05,
                # Boss combat: durability loss prevention
                boss_durability_prevention_rate=0.25,
            )

        if color == "Black":
            return cls(
                color=color,
                land=land,
                swamp_siphon=True,
                swamp_self_tax=2,
                swamp_bankruptcy_games=3,
                # Dig: pure ruin/risk theme — +hazard chance, no siphon on dig yields
                dig_hazard_modifier=0.01,
            )

        # Unknown color: return defaults with only identity set
        return cls(color=color, land=land)
