"""
Service for resolving active mana color effects for a player.

Effects are derived from the player's current mana land assignment (not stored).
Mana changes daily at 4 AM PST, and effects change with it.
"""

import logging
import random
import uuid
from typing import TYPE_CHECKING

from config import HOSTILE_LOSS_MIN_BALANCE
from domain.models.mana_effects import ManaEffects
from services.mana_service import LAND_COLORS, get_today_pst
from utils.economy_scaling import (
    adjust_generated_jc_reward,
    scale_minigame_jc_delta,
)

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
        protection_service=None,
        economy_event_service=None,
    ):
        self.mana_service = mana_service
        self.player_repo = player_repo
        self.mana_repo = mana_repo
        self.loan_service = loan_service
        self.protection_service = protection_service
        self.economy_event_service = economy_event_service

    def get_effects(self, discord_id: int, guild_id: int | None) -> ManaEffects:
        """Get active mana effects for a player.

        Returns ManaEffects with all modifiers set based on current mana color.
        If no mana assigned today, or if today's mana has been tapped on a
        manashop ultimate, returns default (no effects).
        """
        mana = self.mana_service.get_current_mana(discord_id, guild_id)
        return self._effects_from_mana(mana, get_today_pst())

    def get_effects_bulk(
        self,
        discord_ids: list[int],
        guild_id: int | None,
    ) -> dict[int, ManaEffects]:
        """Resolve effects for many players from one guild mana snapshot."""
        unique_ids = list(dict.fromkeys(discord_ids))
        effects = {discord_id: ManaEffects() for discord_id in unique_ids}
        if not unique_ids:
            return effects

        requested_ids = set(unique_ids)
        today = get_today_pst()
        for mana in self.mana_repo.get_all_mana(guild_id):
            discord_id = mana["discord_id"]
            if discord_id in requested_ids:
                effects[discord_id] = self._effects_from_mana(mana, today)
        return effects

    @staticmethod
    def _effects_from_mana(mana: dict | None, today: str) -> ManaEffects:
        if mana is None or mana.get("assigned_date") != today:
            return ManaEffects()
        if mana.get("consumed", mana.get("consumed_today", False)):
            return ManaEffects()

        land = mana.get("land", mana.get("current_land"))
        color = mana.get("color", LAND_COLORS.get(land))
        return ManaEffects.for_color(color, land)

    def apply_bankrupt_stipend(
        self, discord_id: int, guild_id: int | None, land: str
    ) -> int:
        """If the player is bankrupt (balance ≤ 0) and just rolled White mana (Plains),
        deduct up to WHITE_BANKRUPT_STIPEND from the nonprofit fund and credit it.

        Returns the amount actually paid (0 if not eligible or fund empty).
        """
        if land != "Plains":
            return 0
        from config import WHITE_BANKRUPT_STIPEND

        if WHITE_BANKRUPT_STIPEND <= 0:
            return 0

        player = self.player_repo.get_by_id(
            discord_id, self.player_repo.normalize_guild_id(guild_id)
        )
        if player is None or player.jopacoin_balance > 0:
            return 0

        try:
            fund = self.loan_service.get_nonprofit_fund(guild_id)
        except Exception:
            logger.exception("Failed to read nonprofit fund for stipend")
            return 0

        amount = min(WHITE_BANKRUPT_STIPEND, max(fund, 0))
        if amount <= 0:
            return 0

        try:
            self.loan_service.subtract_from_nonprofit_fund(
                guild_id,
                amount,
                source="mana",
                related_type="bankruptcy_stipend",
                related_id=discord_id,
                reason="white mana bankruptcy stipend reserve debit",
                metadata={"amount": amount, "land": land},
            )
        except Exception:
            logger.exception("Failed to deduct stipend from nonprofit fund")
            return 0

        try:
            self.player_repo.add_balance(
                discord_id,
                self.player_repo.normalize_guild_id(guild_id),
                amount,
                source="mana",
                related_type="bankruptcy_stipend",
                reason="white mana bankruptcy stipend",
                metadata={"amount": amount, "land": land},
            )
        except Exception:
            logger.exception("Stipend balance adjust failed; refunding nonprofit")
            try:
                self.loan_service.add_to_nonprofit_fund(
                    guild_id,
                    amount,
                    source="mana",
                    related_type="bankruptcy_stipend",
                    related_id=discord_id,
                    reason="white mana bankruptcy stipend refund",
                    metadata={"amount": amount, "land": land},
                )
            except Exception:
                logger.exception("Stipend refund to nonprofit also failed")
            return 0
        return amount

    def apply_bankrupt_stipends(
        self,
        discord_ids: list[int],
        guild_id: int | None,
    ) -> dict[int, int]:
        """Apply White-mana stipends to a fresh assignment batch atomically."""
        unique_ids = list(dict.fromkeys(discord_ids))
        if not unique_ids:
            return {}

        from config import WHITE_BANKRUPT_STIPEND

        if WHITE_BANKRUPT_STIPEND <= 0:
            return dict.fromkeys(unique_ids, 0)
        try:
            return self.loan_service.distribute_nonprofit_stipends(
                unique_ids,
                guild_id,
                WHITE_BANKRUPT_STIPEND,
            )
        except Exception:
            logger.exception("Failed to apply White stipend batch")
            return dict.fromkeys(unique_ids, 0)

    def execute_siphon(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Execute Swamp's parasitic siphon against a random eligible player.

        Returns dict with siphon details or None if no valid target.
        {
            "victim_id": int,
            "amount": int,  # amount that landed after protection
            "attempted_amount": int,
            "absorbed_amount": int,
            "anonymous": bool,  # True ~60% of time (dark message), False ~40% (mana hint)
        }
        """
        # Pick an eligible victim in SQL (no full table scan). Hostile systems
        # share one minimum-balance policy so a low-balance player is not safe
        # from one attack but unexpectedly exposed to another.
        victim = self.player_repo.get_random_eligible_target(
            guild_id,
            exclude_id=discord_id,
            min_balance=HOSTILE_LOSS_MIN_BALANCE,
        )
        if not victim:
            return None
        amount = scale_minigame_jc_delta(random.randint(1, 3))
        # Don't steal more than they have
        amount = min(amount, victim.jopacoin_balance)
        if amount <= 0:
            return None

        event_key = f"swamp_siphon:{uuid.uuid4().hex}:{victim.discord_id}"

        # Atomic protected steal. The destination is the siphoner, so the
        # gateway keeps victim debit, shield consumption, and thief credit in
        # one transaction.
        try:
            if self.protection_service is not None:
                outcome = self.protection_service.apply_hostile_loss(
                    victim.discord_id,
                    guild_id,
                    amount,
                    kind="swamp_siphon",
                    actor_id=discord_id,
                    event_key=event_key,
                    destination="player",
                    recipient_id=discord_id,
                    clamp_to_balance=True,
                )
                # ``outcome`` is a HostileLossResult; read its fields directly
                # rather than shape-sniffing with a default that would report
                # the full attempted amount on a mismatch.
                applied = int(outcome.applied_loss)
                absorbed = int(outcome.absorbed_amount)
            else:
                self.player_repo.steal_atomic(
                    thief_discord_id=discord_id,
                    victim_discord_id=victim.discord_id,
                    guild_id=guild_id,
                    amount=amount,
                    source="mana",
                    actor_id=discord_id,
                    related_type="hostile_loss",
                    related_id=event_key,
                    reason="swamp siphon",
                    metadata={
                        "kind": "swamp_siphon",
                        "attempted_loss": amount,
                        "destination": "player",
                        "recipient_id": discord_id,
                    },
                )
                applied = amount
                absorbed = 0
        except Exception as e:
            logger.warning("Siphon failed for %s: %s", discord_id, e)
            return None

        # ~60% anonymous, ~40% mana hint
        anonymous = random.random() < 0.6

        return {
            "victim_id": victim.discord_id,
            "amount": applied,
            "attempted_amount": amount,
            "absorbed_amount": absorbed,
            "anonymous": anonymous,
        }

    def apply_blue_tax(self, discord_id: int, guild_id: int | None, gain: int) -> int:
        """Apply Blue's 5.5% tax on JC gains. Returns the tax amount deducted."""
        effects = self.get_effects(discord_id, guild_id)
        if effects.blue_tax_rate <= 0 or gain <= 0:
            return 0
        tax = max(1, int(gain * effects.blue_tax_rate))
        self.player_repo.add_balance(
            discord_id,
            guild_id,
            -tax,
            source="mana",
            related_type="blue_tax",
            reason="blue mana gain tax",
            metadata={"gain": gain, "tax": tax},
        )
        return tax

    def apply_blue_cashback(self, discord_id: int, guild_id: int | None, loss: int) -> int:
        """Apply Blue's 5% cashback on JC losses. Returns the cashback amount added."""
        effects = self.get_effects(discord_id, guild_id)
        if effects.blue_cashback_rate <= 0 or loss <= 0:
            return 0
        base_cashback = max(1, int(abs(loss) * effects.blue_cashback_rate))
        cashback = adjust_generated_jc_reward(
            base_cashback,
            guild_id=guild_id,
            economy_event_service=self.economy_event_service,
        )
        if cashback <= 0:
            return 0
        self.player_repo.add_balance(
            discord_id,
            guild_id,
            cashback,
            source="mana_reward",
            related_type="blue_cashback",
            reason="blue mana loss cashback",
            metadata={
                "loss": loss,
                "base_cashback": base_cashback,
                "adjusted_cashback": cashback,
            },
        )
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
        # Debit the player and credit the nonprofit fund in one atomic transfer so
        # a failure between the two steps cannot destroy (debit-only) or mint
        # (credit-only) the tithed coins.
        self.loan_service.transfer_balance_to_nonprofit(
            discord_id,
            guild_id,
            tithe,
            source="mana",
            related_type="plains_tithe",
            related_id=discord_id,
            reason="plains mana tithe",
            metadata={"gain": gain, "tithe": tithe},
        )
        return tithe

    # ------------------------------------------------------------------
    # New cross-system mana modifiers
    # ------------------------------------------------------------------

    def apply_loan_modifiers(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        base_fee_rate: float,
        base_limit: int,
    ) -> dict:
        """Compute mana-modified loan fee rate and limit.

        Returns {"fee_rate": float, "limit": int, "color": str | None}.
        """
        effects = self.get_effects(discord_id, guild_id)
        if effects.color is None:
            return {"fee_rate": base_fee_rate, "limit": base_limit, "color": None}
        return {
            "fee_rate": max(0.0, base_fee_rate * effects.loan_fee_mult),
            "limit": max(1, int(base_limit * effects.loan_limit_mult)),
            "color": effects.color,
        }

    def apply_sabotage_modifiers(
        self,
        attacker_id: int,
        guild_id: int | None,
        *,
        base_cost: int,
    ) -> dict:
        """Return mana-adjusted sabotage cost and PvP modifiers for attacker.

        {"cost": int, "steal_depth_pct": float, "color": str | None}
        """
        effects = self.get_effects(attacker_id, guild_id)
        if effects.color is None:
            return {
                "cost": base_cost,
                "steal_depth_pct": 0.0,
                "color": None,
            }
        return {
            "cost": max(1, int(base_cost * effects.sabotage_cost_mult)),
            "steal_depth_pct": effects.sabotage_steal_depth_pct,
            "color": effects.color,
        }

    def get_boss_combat_modifiers(
        self, discord_id: int, guild_id: int | None
    ) -> dict:
        """Return per-color boss-combat modifiers for the given player.

        {damage_mult, dynamite_damage_mult, hp_mult, loot_mult, reveal_hp,
         no_crit_against, color}
        """
        effects = self.get_effects(discord_id, guild_id)
        return {
            "damage_mult": effects.boss_damage_mult,
            "dynamite_damage_mult": effects.boss_dynamite_damage_mult,
            "hp_mult": effects.boss_hp_mult,
            "loot_mult": effects.boss_loot_mult,
            "reveal_hp": effects.boss_reveal_hp,
            "no_crit_against": effects.boss_no_crit_against,
            "color": effects.color,
        }

    def get_weather_combo_modifiers(
        self, discord_id: int, guild_id: int | None, weather: str | None
    ) -> dict:
        """Return any active weather × mana combo modifiers for ``weather``.

        ``weather`` is a lowercase code: 'storm', 'sunny', 'fog', 'heat',
        'rain' (or None / unknown). Unknown weather returns identity-only
        results.

        {cooldown_mult, yield_mult, hazard_mult, dynamite_extra_chains,
         recovery_mult, applied: bool}
        """
        effects = self.get_effects(discord_id, guild_id)
        result = {
            "cooldown_mult": 1.0,
            "yield_mult": 1.0,
            "hazard_mult": 1.0,
            "dynamite_extra_chains": 0,
            "recovery_mult": 1.0,
            "applied": False,
        }
        if effects.color is None or not weather:
            return result
        w = weather.lower()
        if w == "storm" and effects.color == "Blue":
            result["cooldown_mult"] = effects.weather_combo_storm_cooldown_mult
            result["applied"] = True
        elif w == "sunny" and effects.color == "White":
            result["yield_mult"] = effects.weather_combo_sunny_yield_mult
            result["applied"] = True
        elif w == "fog" and effects.color == "Black":
            result["hazard_mult"] = effects.weather_combo_fog_hazard_mult
            result["applied"] = True
        elif w == "heat" and effects.color == "Red":
            result["dynamite_extra_chains"] = effects.weather_combo_heat_dynamite_extra_chains
            result["applied"] = True
        elif w == "rain" and effects.color == "Green":
            result["recovery_mult"] = effects.weather_combo_rain_recovery_mult
            result["applied"] = True
        return result
