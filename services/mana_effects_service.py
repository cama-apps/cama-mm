"""
Service for resolving active mana color effects for a player.

Effects are derived from the player's current mana land assignment (not stored).
Mana changes daily at 4 AM PST, and effects change with it.
"""

import logging
import random
from typing import TYPE_CHECKING

from domain.models.mana_effects import ManaEffects
from services.mana_service import get_today_pst

logger = logging.getLogger("cama_bot.services.mana_effects")

if TYPE_CHECKING:
    from repositories.mana_repository import ManaRepository
    from repositories.player_repository import PlayerRepository
    from services.loan_service import LoanService
    from services.mana_service import ManaService


class ManaEffectsService:
    """Resolves active mana effects for a player based on current land."""

    def __init__(
        self,
        mana_service: "ManaService",
        player_repo: "PlayerRepository",
        mana_repo: "ManaRepository",
        loan_service: "LoanService",
    ):
        self.mana_service = mana_service
        self.player_repo = player_repo
        self.mana_repo = mana_repo
        self.loan_service = loan_service

    def get_effects(self, discord_id: int, guild_id: int | None) -> ManaEffects:
        """Get active mana effects for a player.

        Returns ManaEffects with all modifiers set based on current mana color.
        If no mana assigned today, returns default (no effects).
        """
        mana = self.mana_service.get_current_mana(discord_id, guild_id)
        if mana is None:
            return ManaEffects()

        # Only apply effects if mana was assigned today
        today = get_today_pst()
        if mana.get("assigned_date") != today:
            return ManaEffects()

        color = mana.get("color")
        land = mana.get("land")
        return ManaEffects.for_color(color, land)

    def execute_siphon(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Execute Swamp's parasitic siphon: steal 1-3 JC from a random player.

        Returns dict with siphon details or None if no valid target.
        {
            "victim_id": int,
            "amount": int,
            "anonymous": bool,  # True ~60% of time (dark message), False ~40% (mana hint)
        }
        """
        # Pick a random victim with positive balance (SQL-level, no full table scan)
        victim = self.player_repo.get_random_eligible_target(guild_id, exclude_id=discord_id)
        if not victim:
            return None
        amount = random.randint(1, 3)
        # Don't steal more than they have
        amount = min(amount, victim.jopacoin_balance)
        if amount <= 0:
            return None

        # Atomic steal
        try:
            self.player_repo.steal_atomic(
                thief_discord_id=discord_id,
                victim_discord_id=victim.discord_id,
                guild_id=guild_id,
                amount=amount,
            )
        except Exception as e:
            logger.warning("Siphon failed for %s: %s", discord_id, e)
            return None

        # ~60% anonymous, ~40% mana hint
        anonymous = random.random() < 0.6

        return {
            "victim_id": victim.discord_id,
            "amount": amount,
            "anonymous": anonymous,
        }

    def apply_blue_tax(self, discord_id: int, guild_id: int | None, gain: int) -> int:
        """Apply Blue's 5% tax on JC gains. Returns the tax amount deducted."""
        effects = self.get_effects(discord_id, guild_id)
        if effects.blue_tax_rate <= 0 or gain <= 0:
            return 0
        tax = max(1, int(gain * effects.blue_tax_rate))
        self.player_repo.add_balance(discord_id, guild_id, -tax)
        return tax

    def apply_blue_cashback(self, discord_id: int, guild_id: int | None, loss: int) -> int:
        """Apply Blue's 5% cashback on JC losses. Returns the cashback amount added."""
        effects = self.get_effects(discord_id, guild_id)
        if effects.blue_cashback_rate <= 0 or loss <= 0:
            return 0
        cashback = max(1, int(abs(loss) * effects.blue_cashback_rate))
        self.player_repo.add_balance(discord_id, guild_id, cashback)
        return cashback

    def apply_green_cap(self, effects: ManaEffects, gain: int) -> int:
        """Apply Green's gain cap. Returns the capped gain."""
        if effects.green_gain_cap is not None and gain > effects.green_gain_cap:
            return effects.green_gain_cap
        return gain

    def apply_shop_discount(
        self, discord_id: int, guild_id: int | None, base_cost: int, *, kind: str
    ) -> int:
        """Return the effective shop cost after the player's mana discount.

        ``kind`` selects which discount field applies: 'info' for info-style
        items (recalibrate, mystery gift), 'consumable' for dig consumables.
        Returns ``base_cost`` if no discount applies. Always at least 1 JC.
        """
        if base_cost <= 0:
            return base_cost
        effects = self.get_effects(discord_id, guild_id)
        if effects.color is None:
            return base_cost
        rate = 0.0
        if kind == "info":
            rate = effects.shop_info_discount_rate
        elif kind == "consumable":
            rate = effects.shop_consumable_discount_rate
        if rate <= 0:
            return base_cost
        discounted = int(base_cost * (1.0 - rate))
        return max(1, discounted)

    def apply_plains_tithe(self, discord_id: int, guild_id: int | None, gain: int) -> int:
        """Apply Plains' 5% tithe on gains. Tithed JC is transferred to the
        guild's nonprofit fund (not destroyed). Returns the tithe amount."""
        effects = self.get_effects(discord_id, guild_id)
        if effects.plains_tithe_rate <= 0 or gain <= 0:
            return 0
        tithe = max(1, int(gain * effects.plains_tithe_rate))
        self.player_repo.add_balance(discord_id, guild_id, -tithe)
        self.loan_service.add_to_nonprofit_fund(guild_id, tithe)
        return tithe
