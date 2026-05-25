"""ProgressionMixin mixin for :class:`DigService`.

Mana modifiers, weather, prestige perks, miner stats and
profiles, the dig shop, pickaxe upgrades, help, sabotage, and
leaderboard/stats.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import json
import random
import time

from domain.models.dig_gear import GearSlot
from services.dig._common import (
    DIG_BOSS_STAT_POINT_BONUS,
    DIG_STARTING_STAT_POINTS,
    MINER_BACKSTORY_MAX_LENGTH,
    SMARTS_CAVE_IN_REDUCTION,
    STAMINA_COOLDOWN_REDUCTION,
    STAMINA_MAX_REDUCTION,
    STRENGTH_MAX_ADVANCE_INTERVAL,
    STRENGTH_MIN_ADVANCE_INTERVAL,
    logger,
)
from services.dig_constants import (
    CONSUMABLE_ITEMS,
    DIG_TIPS,
    FREE_DIG_COOLDOWN,
    GEAR_TIER_TABLES,
    HELLTIDE_MODIFIER_ID,
    HELLTIDE_TAX_PER_DIG,
    LAYER_WEATHER_POOL,
    MILESTONES,
    PAID_DIG_COSTS,
    PICKAXE_TIERS,
    PRESTIGE_PERK_STACK_CAP,
    PRESTIGE_PERK_VALUES,
    PRESTIGE_PERKS,
    WEATHER_BY_ID,
)


class ProgressionMixin:
    """ProgressionMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """
    def _apply_mana_yield_variance(
        self, discord_id: int, guild_id, base_jc: int
    ) -> int:
        """Apply Mountain variance + Forest steady bonus to a base loot roll.

        These mods touch only the random base loot — deterministic milestone
        and streak bonuses are added afterwards so a Mountain "zero" roll never
        wipes out a payout the embed already promised the player.
        """
        if base_jc <= 0:
            return base_jc
        effects = self._mana_effects_or_none(discord_id, guild_id)
        if effects is None:
            return base_jc

        modified = base_jc

        # Mountain variance: chance of double or zero (same EV).
        if effects.dig_yield_variance > 0:
            roll = random.random()
            if roll < effects.dig_yield_variance:
                modified = modified * 2
            elif roll < effects.dig_yield_variance * 2:
                modified = 0

        # Forest +1 steady bonus on any positive yield.
        if effects.green_steady_bonus > 0 and modified > 0:
            modified += effects.green_steady_bonus

        return max(0, modified)

    def _helltide_tax(self, guild_id) -> int:
        """Per-dig flat JC tax while the helltide bell is active in this guild.

        Returns 0 when the modifier repo isn't wired or the modifier has
        expired. The tax is destroyed (deflation), not transferred.
        """
        if self.dig_guild_modifier_repo is None:
            return 0
        try:
            if self.dig_guild_modifier_repo.is_active(guild_id, HELLTIDE_MODIFIER_ID):
                return HELLTIDE_TAX_PER_DIG
        except Exception:
            return 0
        return 0

    def _apply_mana_yield_taxes(
        self, discord_id: int, guild_id, total_jc: int
    ) -> int:
        """Apply Plains tithe + Blue tax to the player's full dig payout.

        Tax/tithe apply to the *total* (base + milestone + streak) so the
        deflationary pressure matches /roll and /betting paths. Plains tithed
        JC is forwarded to the nonprofit fund (the helper bypasses
        ``apply_plains_tithe`` because the JC hasn't been credited to the
        player's balance yet — atomic_tunnel_balance_update applies the net
        delta later in the dig flow).
        """
        if total_jc <= 0:
            return total_jc
        effects = self._mana_effects_or_none(discord_id, guild_id)
        if effects is None:
            return total_jc

        modified = total_jc

        # Plains tithe: transfer 5% to nonprofit fund. Skip if the transfer
        # fails — never destroy JC silently.
        loan_service = getattr(self.mana_effects_service, "loan_service", None)
        if effects.plains_tithe_rate > 0 and modified > 0 and loan_service is not None:
            tithe = max(1, int(modified * effects.plains_tithe_rate))
            try:
                loan_service.add_to_nonprofit_fund(guild_id, tithe)
                modified -= tithe
            except Exception:
                logger.warning(
                    "Plains tithe transfer failed in dig; tithe skipped "
                    "rather than destroying JC.",
                    exc_info=True,
                )

        # Blue tax on positive yields (deflationary, JC destroyed by design).
        if effects.blue_tax_rate > 0 and modified > 0:
            tax = max(1, int(modified * effects.blue_tax_rate))
            modified -= tax

        return max(0, modified)

    def _apply_mana_paid_cost_modifier(
        self, discord_id: int, guild_id, base_cost: int
    ) -> int:
        """Silently apply the player's mana paid-cost modifier (Mountain -5%)."""
        if self.mana_effects_service is None or base_cost <= 0:
            return base_cost
        try:
            effects = self.mana_effects_service.get_effects(discord_id, guild_id)
        except Exception:
            return base_cost
        if effects.color is None or effects.dig_paid_cost_modifier_pct == 0:
            return base_cost
        adjusted = int(base_cost * (1.0 + effects.dig_paid_cost_modifier_pct))
        return max(1, adjusted)

    def _apply_mana_cooldown_reduction(
        self, discord_id: int, guild_id, cooldown_seconds: int
    ) -> int:
        """Silently apply the player's mana cooldown reduction (Forest -30s)."""
        if self.mana_effects_service is None or cooldown_seconds <= 0:
            return cooldown_seconds
        try:
            effects = self.mana_effects_service.get_effects(discord_id, guild_id)
        except Exception:
            return cooldown_seconds
        if effects.color is None or effects.dig_cooldown_reduction_seconds <= 0:
            return cooldown_seconds
        return max(1, cooldown_seconds - effects.dig_cooldown_reduction_seconds)

    def _apply_mana_hazard_modifier(
        self, discord_id: int, guild_id, base_chance: float
    ) -> float:
        """Silently shift cave-in probability by the player's mana modifier.

        Forest reduces, Mountain/Black raise. Clamped to [0, 1].
        """
        if self.mana_effects_service is None:
            return base_chance
        try:
            effects = self.mana_effects_service.get_effects(discord_id, guild_id)
        except Exception:
            return base_chance
        if effects.color is None or effects.dig_hazard_modifier == 0:
            return base_chance
        return max(0.0, min(1.0, base_chance + effects.dig_hazard_modifier))

    def _roll_weather(self, guild_id) -> list[dict]:
        """Roll 2 weather events for today, targeting populated layers.

        Returns list of dicts with layer_name and weather_id.
        """
        tunnels = self.dig_repo.get_all_tunnels(guild_id)

        # Count players per layer (only tunnels active in last 7 days)
        cutoff = int(time.time()) - 7 * 86400
        layer_pop: dict[str, int] = {}
        for t in tunnels:
            if (t.get("last_dig_at") or 0) >= cutoff:
                layer_name = self._get_layer(t.get("depth", 0)).get("name", "Dirt")
                layer_pop[layer_name] = layer_pop.get(layer_name, 0) + 1

        all_layers = list(LAYER_WEATHER_POOL.keys())
        populated = [ly for ly in all_layers if layer_pop.get(ly, 0) > 0]

        picks = []

        # First pick: guaranteed populated layer (weighted by population)
        if populated:
            weights = [layer_pop[ly] for ly in populated]
            first_layer = random.choices(populated, weights=weights, k=1)[0]
        else:
            first_layer = random.choice(all_layers)
        weather = random.choice(LAYER_WEATHER_POOL[first_layer])
        picks.append({"layer_name": first_layer, "weather_id": weather.id})

        # Second pick: any populated layer (or random if < 2 populated)
        remaining_pop = [ly for ly in populated if ly != first_layer]
        if remaining_pop:
            second_layer = random.choice(remaining_pop)
        else:
            remaining = [ly for ly in all_layers if ly != first_layer]
            second_layer = random.choice(remaining)
        weather2 = random.choice(LAYER_WEATHER_POOL[second_layer])
        picks.append({"layer_name": second_layer, "weather_id": weather2.id})

        return picks

    def _ensure_weather(self, guild_id) -> list[dict]:
        """Lazily roll weather for today if not already set. Returns active weather."""
        today = self._get_game_date()
        existing = self.dig_repo.get_weather(guild_id, today)
        if existing:
            return existing

        picks = self._roll_weather(guild_id)
        for pick in picks:
            self.dig_repo.set_weather(guild_id, today, pick["layer_name"], pick["weather_id"])

        return self.dig_repo.get_weather(guild_id, today)

    def get_weather(self, guild_id) -> list[dict]:
        """Public: get today's weather with full info for display."""
        entries = self._ensure_weather(guild_id)
        result = []
        for entry in entries:
            w = WEATHER_BY_ID.get(entry.get("weather_id"))
            if w:
                result.append({
                    "layer": w.layer,
                    "name": w.name,
                    "description": w.description,
                    "effects": w.effects,
                })
        return result

    def _get_weather_effects(self, guild_id, layer_name: str) -> dict:
        """Get combined weather effects for a specific layer today."""
        entries = self._ensure_weather(guild_id)
        for entry in entries:
            if entry.get("layer_name") == layer_name:
                w = WEATHER_BY_ID.get(entry.get("weather_id"))
                if w:
                    return dict(w.effects)
        return {}

    def _get_weather_code(self, guild_id, layer_name: str) -> str | None:
        """Return the active weather id for ``layer_name`` (lowercase
        keyword like 'storm', 'sunny', 'fog', 'heat', 'rain') or None.

        Used by relics + mana × weather combos that key off the *kind* of
        weather rather than its mechanical effects.
        """
        entries = self._ensure_weather(guild_id)
        for entry in entries:
            if entry.get("layer_name") == layer_name:
                wid = entry.get("weather_id")
                if wid is None:
                    return None
                return str(wid).lower()
        return None

    def _get_prestige_perks(self, tunnel: dict) -> list[str]:
        """Get list of active prestige perks."""
        raw = tunnel.get("prestige_perks")
        if not raw:
            return []
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return []

    def _aggregate_perk_effects(self, perks: list[str]) -> dict[str, float]:
        """Sum mechanical effects across all picked perks.

        Each entry in ``PRESTIGE_PERK_VALUES[perk]`` contributes its
        keys; duplicates (same perk picked across multiple prestiges)
        sum naturally. Lookup returns 0.0 / falsy for missing keys.
        """
        aggregated: dict[str, float] = {}
        for perk in perks:
            for key, value in PRESTIGE_PERK_VALUES.get(perk, {}).items():
                aggregated[key] = aggregated.get(key, 0.0) + float(value)
        return aggregated

    def _eligible_perks(self, tunnel: dict) -> list[str]:
        """Perks the player can still pick — duplicates allowed up to the cap."""
        owned = self._get_prestige_perks(tunnel)
        return [p for p in PRESTIGE_PERKS if owned.count(p) < PRESTIGE_PERK_STACK_CAP]

    def has_perk(self, discord_id: int, guild_id, perk_id: str) -> bool:
        """True if the player owns the given prestige perk."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if not tunnel:
            return False
        return perk_id in self._get_prestige_perks(dict(tunnel))

    def _get_miner_stats(self, tunnel: dict) -> dict:
        """Return normalized miner S stats and available point budget."""
        strength = max(0, int(tunnel.get("stat_strength") or 0))
        smarts = max(0, int(tunnel.get("stat_smarts") or 0))
        stamina = max(0, int(tunnel.get("stat_stamina") or 0))
        total_points = max(
            DIG_STARTING_STAT_POINTS,
            int(tunnel.get("stat_points") or DIG_STARTING_STAT_POINTS),
        )
        spent = strength + smarts + stamina
        return {
            "strength": strength,
            "smarts": smarts,
            "stamina": stamina,
            "stat_points": total_points,
            "spent_points": spent,
            "unspent_points": max(0, total_points - spent),
        }

    def _get_stat_effects(self, stats: dict) -> dict:
        """Translate S stats into mechanical dig modifiers."""
        strength = stats.get("strength", 0)
        smarts = stats.get("smarts", 0)
        stamina = stats.get("stamina", 0)
        stamina_reduction = min(STAMINA_MAX_REDUCTION, stamina * STAMINA_COOLDOWN_REDUCTION)
        return {
            "advance_min_bonus": strength // STRENGTH_MIN_ADVANCE_INTERVAL,
            "advance_max_bonus": strength // STRENGTH_MAX_ADVANCE_INTERVAL,
            "cave_in_reduction": smarts * SMARTS_CAVE_IN_REDUCTION,
            "cooldown_multiplier": 1.0 - stamina_reduction,
            "paid_cost_multiplier": 1.0 - stamina_reduction,
        }

    def _apply_stamina_to_cooldown(self, cooldown: int, tunnel: dict) -> int:
        stats = self._get_miner_stats(tunnel)
        effects = self._get_stat_effects(stats)
        return max(1, int(cooldown * effects["cooldown_multiplier"]))

    def _apply_stamina_to_paid_cost(self, cost: int, tunnel: dict) -> int:
        stats = self._get_miner_stats(tunnel)
        effects = self._get_stat_effects(stats)
        return max(1, int(cost * effects["paid_cost_multiplier"]))

    def _calculate_paid_dig_cost(self, tunnel: dict, paid_count: int) -> int:
        cost_index = min(paid_count, len(PAID_DIG_COSTS) - 1)
        paid_dig_cost = PAID_DIG_COSTS[cost_index]
        prestige_lvl = tunnel.get("prestige_level", 0) or 0
        asc = self._get_ascension_effects(prestige_lvl)
        if asc.get("paid_dig_cost_multiplier"):
            paid_dig_cost = int(paid_dig_cost * (1 + asc["paid_dig_cost_multiplier"]))
        return self._apply_stamina_to_paid_cost(paid_dig_cost, tunnel)

    def _sanitize_miner_text(self, value: str | None, max_length: int) -> str:
        if value is None:
            return ""
        clean = " ".join(str(value).replace("@", "(at)").split())
        return clean[:max_length]

    def _has_locked_backstory(self, tunnel: dict) -> bool:
        return bool((tunnel.get("miner_about") or "").strip())

    def _ensure_tunnel_for_profile(self, discord_id: int, guild_id) -> dict:
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            self.dig_repo.create_tunnel(
                discord_id, guild_id, name=self.generate_tunnel_name()
            )
            tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        return dict(tunnel)

    def _get_stat_boss_awards(self, tunnel: dict) -> list[int]:
        raw = tunnel.get("stat_boss_awards")
        if not raw:
            return []
        try:
            decoded = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return []
        if not isinstance(decoded, list):
            return []
        awards = []
        for value in decoded:
            try:
                awards.append(int(value))
            except (TypeError, ValueError):
                continue
        return awards

    def _award_boss_stat_point_if_first(
        self, discord_id: int, guild_id, tunnel: dict, boundary: int
    ) -> bool:
        """Award one S-stat point the first time this boss is fully defeated."""
        awarded = self._get_stat_boss_awards(tunnel)
        if boundary in awarded:
            return False
        awarded.append(boundary)
        current_points = max(
            DIG_STARTING_STAT_POINTS,
            int(tunnel.get("stat_points") or DIG_STARTING_STAT_POINTS),
        )
        self.dig_repo.update_tunnel(
            discord_id,
            guild_id,
            stat_points=current_points + DIG_BOSS_STAT_POINT_BONUS,
            stat_boss_awards=json.dumps(sorted(set(awarded))),
        )
        tunnel["stat_points"] = current_points + DIG_BOSS_STAT_POINT_BONUS
        tunnel["stat_boss_awards"] = json.dumps(sorted(set(awarded)))
        return True

    def get_miner_profile(self, discord_id: int, guild_id) -> dict:
        """Return the player's dig profile and S-stat effects."""
        if not self.player_repo.exists(discord_id, guild_id):
            return self._error("You need to register first. Use /player register.")
        tunnel = self._ensure_tunnel_for_profile(discord_id, guild_id)
        stats = self._get_miner_stats(tunnel)
        effects = self._get_stat_effects(stats)
        return self._ok(
            backstory=tunnel.get("miner_about") or "",
            stats=stats,
            effects=effects,
            awarded_bosses=self._get_stat_boss_awards(tunnel),
        )

    def set_miner_profile(
        self,
        discord_id: int,
        guild_id,
        *,
        backstory: str | None = None,
    ) -> dict:
        """Set the player's miner backstory once."""
        if not self.player_repo.exists(discord_id, guild_id):
            return self._error("You need to register first. Use /player register.")
        tunnel = self._ensure_tunnel_for_profile(discord_id, guild_id)
        if self._has_locked_backstory(tunnel):
            return self._error("Your miner backstory is already set and cannot be changed.")
        story = self._sanitize_miner_text(backstory, MINER_BACKSTORY_MAX_LENGTH)
        if not story:
            return self._error("Provide a backstory to lock in.")
        self.dig_repo.update_tunnel(discord_id, guild_id, miner_about=story)
        tunnel["miner_about"] = story
        return self._ok(
            backstory=tunnel.get("miner_about") or "",
        )

    def set_miner_stats(
        self,
        discord_id: int,
        guild_id,
        *,
        strength: int,
        smarts: int,
        stamina: int,
    ) -> dict:
        """Allocate additional S-stat points without allowing respecs."""
        if not self.player_repo.exists(discord_id, guild_id):
            return self._error("You need to register first. Use /player register.")
        tunnel = self._ensure_tunnel_for_profile(discord_id, guild_id)
        try:
            values = {
                "strength": int(strength),
                "smarts": int(smarts),
                "stamina": int(stamina),
            }
        except (TypeError, ValueError):
            return self._error("S stats must be whole numbers.")
        if any(v < 0 for v in values.values()):
            return self._error("S stats cannot be negative.")
        if not any(values.values()):
            return self._error("Spend at least one point.")
        stats = self._get_miner_stats(tunnel)
        total = sum(values.values())
        if total > stats["unspent_points"]:
            return self._error(
                f"That spends {total} points, but you only have {stats['unspent_points']} unspent."
            )
        next_values = {
            "stat_strength": stats["strength"] + values["strength"],
            "stat_smarts": stats["smarts"] + values["smarts"],
            "stat_stamina": stats["stamina"] + values["stamina"],
        }
        self.dig_repo.update_tunnel(
            discord_id,
            guild_id,
            **next_values,
        )
        updated = {
            **tunnel,
            **next_values,
        }
        updated_stats = self._get_miner_stats(updated)
        return self._ok(
            stats=updated_stats,
            effects=self._get_stat_effects(updated_stats),
        )

    def _pick_tip(self, depth: int) -> str:
        """Pick a progressive tip based on current depth."""
        eligible = [
            t for t in DIG_TIPS
            if depth >= t.get("min_depth", 0)
            and (t.get("max_depth") is None or depth <= t["max_depth"])
        ]
        if not eligible:
            return "Keep digging!"
        return random.choice(eligible)["text"]

    # ------------------------------------------------------------------
    # Luminosity
    # ------------------------------------------------------------------

    def calculate_decay(self, discord_id: int, guild_id) -> int:
        """Public wrapper: calculate how much decay would occur, return blocks lost.

        Also applies the decay to the tunnel.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return 0
        tunnel = dict(tunnel)
        result = self._apply_lazy_decay(tunnel, guild_id)
        return result.get("amount", 0)

    def get_shop(self, discord_id: int, guild_id) -> dict:
        """Return shop data: consumables, pickaxe upgrades, gear, inventory count."""
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        inv_count = len(inventory) if inventory else 0

        consumables = [
            {"name": v["name"], "price": v["cost"], "description": v["description"]}
            for v in CONSUMABLE_ITEMS.values()
        ]

        # Show next available pickaxe upgrades
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        current_tier = 0
        if tunnel:
            current_tier = self._get_active_pickaxe_tier(discord_id, guild_id, dict(tunnel))

        pickaxe_upgrades = []
        for i in range(current_tier + 1, len(PICKAXE_TIERS)):
            t = PICKAXE_TIERS[i]
            pickaxe_upgrades.append({
                "name": t["name"],
                "price": t["jc_cost"],
                "depth_req": t["depth_required"],
                "prestige_req": t.get("prestige_required", 0),
            })

        # Show shop-buyable boss gear (tiers 0..3 — Wooden/Stone/Iron/Diamond
        # for armor and boots; weapons remain on the pickaxe ladder above).
        gear_for_sale: list[dict] = []
        for slot_enum, table in GEAR_TIER_TABLES.items():
            if slot_enum == GearSlot.WEAPON:
                continue  # weapons sell via the pickaxe upgrade row
            for tier_idx, td in enumerate(table):
                if tier_idx > 3:
                    continue  # Obsidian+ are drop-only
                if td.shop_price <= 0:
                    continue  # tier 0 is the free starter — never in the shop
                gear_for_sale.append({
                    "slot": slot_enum.value,
                    "tier": tier_idx,
                    "name": td.name,
                    "price": td.shop_price,
                    "depth_req": td.depth_required,
                    "prestige_req": td.prestige_required,
                })

        return self._ok(
            consumables=consumables,
            pickaxe_upgrades=pickaxe_upgrades,
            gear_for_sale=gear_for_sale,
            inventory_count=inv_count,
        )

    def get_upgrade_info(self, discord_id: int, guild_id) -> dict:
        """Return info about current and next pickaxe tier."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._ok(current_tier="Wooden", current_tier_index=0, next_tier=None, eligible=False)

        tunnel = dict(tunnel)
        current_idx = self._get_active_pickaxe_tier(discord_id, guild_id, tunnel)
        current_name = PICKAXE_TIERS[current_idx]["name"] if current_idx < len(PICKAXE_TIERS) else "Unknown"

        if current_idx >= len(PICKAXE_TIERS) - 1:
            return self._ok(current_tier=current_name, current_tier_index=current_idx, next_tier=None, eligible=False)

        next_tier = PICKAXE_TIERS[current_idx + 1]
        depth = tunnel.get("depth", 0)
        prestige = tunnel.get("prestige_level", 0)
        balance = self.player_repo.get_balance(discord_id, guild_id)

        missing = []
        if depth < next_tier["depth_required"]:
            missing.append(f"Depth {next_tier['depth_required']} (have {depth})")
        if prestige < next_tier.get("prestige_required", 0):
            missing.append(f"Prestige {next_tier['prestige_required']} (have {prestige})")
        if balance < next_tier["jc_cost"]:
            missing.append(f"{next_tier['jc_cost']} JC (have {balance})")

        return self._ok(
            current_tier=current_name,
            current_tier_index=current_idx,
            next_tier=next_tier["name"],
            cost=next_tier["jc_cost"],
            depth_required=next_tier["depth_required"],
            prestige_required=next_tier.get("prestige_required", 0),
            eligible=len(missing) == 0,
            missing_requirements=missing,
        )

    def preview_sabotage(self, actor_id: int, target_id: int, guild_id) -> dict:
        """Preview sabotage cost and damage range without executing."""
        if actor_id == target_id:
            return self._error("You can't sabotage yourself.")

        target_tunnel = self.dig_repo.get_tunnel(target_id, guild_id)
        if target_tunnel is None:
            return self._error("That player doesn't have a tunnel.")

        target_depth = dict(target_tunnel).get("depth", 0)
        cost = max(5, target_depth // 5)

        return self._ok(cost=cost, damage_range="3-8", target_depth=target_depth)

    def upgrade_pickaxe_to_tier(
        self, discord_id: int, guild_id, target_tier: int,
    ) -> dict:
        """Upgrade the pickaxe to the given tier — must be exactly current+1.

        Backs ``/dig buy weapon:N``. Reuses the same gating as
        ``upgrade_pickaxe`` (depth, prestige, JC cost) by delegating once the
        target matches current+1. Rejects non-sequential targets so players
        cannot tier-skip through the dig command surface.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")
        current_tier = self._get_active_pickaxe_tier(
            discord_id, guild_id, dict(tunnel),
        )
        if target_tier != current_tier + 1:
            if target_tier <= current_tier:
                return self._error("You already have that pickaxe tier or higher.")
            next_name = (
                PICKAXE_TIERS[current_tier + 1]["name"]
                if current_tier + 1 < len(PICKAXE_TIERS) else "next tier"
            )
            return self._error(
                f"Buy {next_name} first — pickaxes upgrade one tier at a time."
            )
        return self.upgrade_pickaxe(discord_id, guild_id)

    def upgrade_pickaxe(self, discord_id: int, guild_id) -> dict:
        """Upgrade pickaxe to next tier if requirements met.

        Writes to BOTH the legacy ``tunnels.pickaxe_tier`` column and the new
        ``dig_gear`` Weapon row so older read-paths (e.g. saboteur clue,
        leaderboard rendering) keep working through the migration window.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        current_tier = self._get_active_pickaxe_tier(discord_id, guild_id, tunnel)

        if current_tier >= len(PICKAXE_TIERS) - 1:
            return self._error("Already at max pickaxe tier.")

        next_tier_idx = current_tier + 1
        next_tier = PICKAXE_TIERS[next_tier_idx]

        # Check depth requirement
        if tunnel.get("depth", 0) < next_tier.get("depth_required", 0):
            return self._error(
                f"Need depth {next_tier['depth_required']} (you have {tunnel.get('depth', 0)})."
            )

        # Check prestige requirement
        if tunnel.get("prestige_level", 0) < next_tier.get("prestige_required", 0):
            return self._error(
                f"Need prestige level {next_tier['prestige_required']}."
            )

        # Check JC cost
        cost = next_tier.get("jc_cost", 0)
        balance = self.player_repo.get_balance(discord_id, guild_id)
        if balance < cost:
            return self._error(f"Costs {cost} JC but you only have {balance} JC.")

        # Debit + tunnel pickaxe_tier flip commit together so a crash between
        # the two cannot charge the player with no upgrade applied.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-cost,
            tunnel_updates={"pickaxe_tier": next_tier_idx},
        )
        # Mirror the upgrade into dig_gear so equipped weapon stays in sync.
        # These writes are not folded into the atomic block above: the gear
        # tables have their own equip-uniqueness invariants and at most we
        # leak an unequipped weapon row on a crash here, which is harmless.
        equipped = self.dig_repo.get_equipped_gear(discord_id, guild_id)
        old_weapon = equipped.get("weapon")
        if old_weapon is not None:
            self.dig_repo.unequip_gear(int(old_weapon["id"]))
        new_id = self.dig_repo.add_gear(
            discord_id, guild_id, "weapon", next_tier_idx, source="shop",
        )
        self.dig_repo.equip_gear(new_id, discord_id, guild_id, "weapon")

        return self._ok(
            tier=next_tier_idx,
            name=next_tier.get("name", f"Tier {next_tier_idx}"),
            cost=cost,
            balance_after=balance - cost,
        )

    # ------------------------------------------------------------------
    # Help Tunnel
    # ------------------------------------------------------------------

    def help_tunnel(self, helper_id: int, target_id: int, guild_id) -> dict:
        """
        Help another player dig their tunnel.

        Returns: success, error, advance, target_tunnel, helper_cooldown_until.
        """
        if helper_id == target_id:
            return self._error("You can't help yourself.")

        # Check helper cooldown
        helper_tunnel = self.dig_repo.get_tunnel(helper_id, guild_id)
        if helper_tunnel:
            helper_tunnel = dict(helper_tunnel)
            helper_tunnel["discord_id"] = helper_id
            cooldown = self._get_cooldown_remaining(helper_tunnel)
            if cooldown > 0:
                return self._error(f"You're on cooldown ({cooldown}s remaining).")

        # Check target has a tunnel
        target_tunnel = self.dig_repo.get_tunnel(target_id, guild_id)
        if target_tunnel is None:
            return self._error("That player doesn't have a tunnel.")

        target_tunnel = dict(target_tunnel)
        target_tunnel["discord_id"] = target_id

        # Apply lazy decay
        self._apply_lazy_decay(target_tunnel, guild_id)

        target_depth = target_tunnel.get("depth", 0)
        layer = self._get_layer(target_depth)

        # Roll advance
        base_min = layer.get("advance_min", 1)
        base_max = layer.get("advance_max", 5)
        advance = random.randint(base_min, base_max)
        # Relic: Mycelium Link — helper amplifies the advance they grant
        if self._has_relic(helper_id, guild_id, "mycelium_link"):
            advance += 1

        # Cap at boss boundary
        boss_progress = self._get_boss_progress(target_tunnel)
        next_boss = self._next_boss_boundary(target_depth, boss_progress)
        if next_boss is not None and target_depth + advance >= next_boss:
            advance = max(0, next_boss - 1 - target_depth)

        new_depth = target_depth + advance

        # Relic: Mentor's Lantern — helper grants both sides a JC bump.
        helper_jc_bonus = 1  # baseline help reward
        target_jc_bonus = 0
        mentor_active = self._has_relic(helper_id, guild_id, "mentors_lantern")
        if mentor_active:
            helper_jc_bonus += 10
            target_jc_bonus = 10

        # Target depth + helper cooldown + helper reward + audit log commit
        # together. The old flow committed each step individually and could
        # leave the target advanced with no cooldown tracked, or the helper
        # credited with no cooldown set.
        now = int(time.time())
        self.dig_repo.atomic_help_tunnel(
            helper_id=helper_id,
            target_id=target_id,
            guild_id=guild_id,
            new_target_depth=new_depth,
            helper_last_dig_at=now,
            helper_reward=helper_jc_bonus,
            create_helper_tunnel_name=None if helper_tunnel else self.generate_tunnel_name(),
            log_detail={
                "target_id": target_id, "advance": advance,
                "target_depth_before": target_depth, "target_depth_after": new_depth,
                "mentor_bonus": mentor_active,
            },
        )
        if target_jc_bonus > 0:
            try:
                self.player_repo.add_balance(target_id, guild_id, target_jc_bonus)
            except Exception:
                logger.debug("Mentor's Lantern target bonus failed", exc_info=True)
                target_jc_bonus = 0

        return self._ok(
            advance=advance,
            target_tunnel=target_tunnel.get("tunnel_name", "Unknown Tunnel"),
            target_depth_after=new_depth,
            helper_cooldown_until=now + FREE_DIG_COOLDOWN,
            mentor_helper_bonus=helper_jc_bonus if mentor_active else 0,
            mentor_target_bonus=target_jc_bonus,
        )

    # ------------------------------------------------------------------
    # Sabotage
    # ------------------------------------------------------------------

    def sabotage_tunnel(self, actor_id: int, target_id: int, guild_id) -> dict:
        """
        Sabotage another player's tunnel.

        Returns: success, error, cost, damage, target_tunnel,
                 trap_triggered, clue, is_reveal.
        """
        if actor_id == target_id:
            return self._error("You can't sabotage yourself.")

        target_tunnel = self.dig_repo.get_tunnel(target_id, guild_id)
        if target_tunnel is None:
            return self._error("That player doesn't have a tunnel.")

        target_tunnel = dict(target_tunnel)
        target_tunnel["discord_id"] = target_id
        target_depth = target_tunnel.get("depth", 0)

        # Cost
        cost = max(5, target_depth // 5)
        # Mana modifier on attacker (Red halves cost)
        if self.mana_effects_service is not None:
            try:
                _sab_mod = self.mana_effects_service.apply_sabotage_modifiers(
                    actor_id, guild_id, base_cost=cost,
                )
                cost = _sab_mod["cost"]
            except Exception:
                pass
        balance = self.player_repo.get_balance(actor_id, guild_id)
        if balance < cost:
            return self._error(f"Sabotage costs {cost} JC but you only have {balance} JC.")

        # PvP immunity buffs on the target absorb the entire sabotage attempt.
        # Counterspell / Sanctuary block outright; a single Aegis charge is
        # consumed when present. In both cases the would-be victim gets a
        # small JC tip — defending feels like a win, not a non-event.
        victim_block_tip = max(25, cost // 2)
        if self.buff_service is not None:
            try:
                # White mana passive: lazily grant the first-sabotage-today aegis
                # if the target has the effect active and the buff hasn't been
                # issued yet today (idempotent — grant is a no-op when a live row
                # already exists).
                if self.mana_effects_service is not None:
                    try:
                        _target_effects = self.mana_effects_service.get_effects(
                            target_id, guild_id
                        )
                        if _target_effects.sabotage_first_aegis_today:
                            from services.buff_service import BUFF_FIRST_AEGIS_TODAY
                            if not self.buff_service.buff_repo.has_active(
                                target_id, guild_id, BUFF_FIRST_AEGIS_TODAY
                            ):
                                self.buff_service.grant_first_aegis_today(target_id, guild_id)
                    except Exception:
                        logger.debug("White mana aegis grant failed", exc_info=True)

                if self.buff_service.has_pvp_immunity(target_id, guild_id):
                    self.player_repo.add_balance(
                        target_id, guild_id, victim_block_tip,
                    )
                    return self._error(
                        "Your target is shielded by an active manashop ward — "
                        "the sabotage was repelled before you could land it. "
                        f"They pocketed {victim_block_tip} JC for the trouble."
                    )
                if self.buff_service.consume_aegis_charge(target_id, guild_id):
                    # Charge absorbed — log the wasted attempt and refund cost.
                    self.player_repo.add_balance(
                        target_id, guild_id, victim_block_tip,
                    )
                    self.dig_repo.log_action(
                        discord_id=actor_id, guild_id=guild_id,
                        action_type="sabotage",
                        details=json.dumps({
                            "target_id": target_id, "absorbed": True, "cost": 0,
                            "victim_tip": victim_block_tip,
                        }),
                    )
                    return self._ok(
                        cost=0,
                        damage=0,
                        target_tunnel=target_tunnel.get("tunnel_name", "Unknown Tunnel"),
                        trap_triggered=False,
                        trap_detail=None,
                        clue=None,
                        is_reveal=False,
                        insurance_applied=False,
                        damage_reduced=True,
                        absorbed_by_aegis=True,
                        victim_tip=victim_block_tip,
                    )
            except Exception:
                logger.debug("PvP immunity / aegis check failed", exc_info=True)

        # 12h cooldown per target
        recent_sabotages = self.dig_repo.get_recent_actions(
            actor_id, guild_id, action_type="sabotage", hours=12
        )
        for sab in recent_sabotages:
            try:
                sab_detail = json.loads(sab.get("detail") or sab.get("details") or "{}")
            except (json.JSONDecodeError, TypeError):
                sab_detail = {}
            if sab_detail.get("target_id") == target_id:
                return self._error("You already sabotaged this player in the last 12 hours.")

        # Check for active trap. Trap victim already gets the attacker's
        # cost as JC; on top of that, add a small block-defense tip so
        # defending a sabotage attempt has a small positive payout.
        if target_tunnel.get("trap_active"):
            trap_steal = cost * 2
            actor_tunnel = self.dig_repo.get_tunnel(actor_id, guild_id)
            actor_loss = random.randint(3, 5)
            trap_victim_tip = victim_block_tip

            self.dig_repo.atomic_sabotage(
                actor_id=actor_id,
                target_id=target_id,
                guild_id=guild_id,
                target_depth_delta=0,
                actor_jc_cost=trap_steal,
                target_jc_credit=cost + trap_victim_tip,
                actor_depth_delta=-actor_loss if actor_tunnel else 0,
                clear_target_trap=True,
                log_detail={
                    "target_id": target_id, "trap_triggered": True,
                    "jc_lost": trap_steal, "blocks_lost": actor_loss,
                    "victim_tip": trap_victim_tip,
                },
            )

            return self._ok(
                cost=trap_steal,
                damage=0,
                target_tunnel=target_tunnel.get("tunnel_name", "Unknown Tunnel"),
                trap_triggered=True,
                trapped=True,
                trap_detail={
                    "jc_lost": trap_steal,
                    "blocks_lost": actor_loss,
                    "message": f"Trap triggered! You lost {trap_steal} JC and {actor_loss} blocks!",
                },
                clue=None,
                is_reveal=False,
            )

        # Calculate damage
        damage = random.randint(3, 8)

        # Reductions
        total_reduction = 0.0

        # Insurance
        insured_until = target_tunnel.get("insured_until") or 0
        now = int(time.time())
        if now < insured_until:
            total_reduction += 0.50

        # Reinforcement
        reinforced_until = target_tunnel.get("reinforced_until") or 0
        if now < reinforced_until:
            total_reduction += 0.25

        # Obsidian Shield relic
        if self._has_relic(target_id, guild_id, "obsidian_shield"):
            total_reduction += 0.15

        # Cap reduction
        total_reduction = min(0.70, total_reduction)
        damage = max(1, int(damage * (1.0 - total_reduction)))

        # Generate clue about saboteur (read-only)
        clue_types = ["first_letter", "depth_range", "pickaxe_tier"]
        clue_type = random.choice(clue_types)
        clue = self._generate_clue(actor_id, guild_id, clue_type)

        # Count prior same-target sabotages (excluding the current one; logged below)
        all_sabotages = self.dig_repo.get_recent_actions(
            actor_id, guild_id, action_type="sabotage", hours=168  # 7 days
        )
        same_target_count = 0
        for sab in all_sabotages:
            try:
                sab_d = json.loads(sab.get("detail") or sab.get("details") or "{}")
            except (json.JSONDecodeError, TypeError):
                sab_d = {}
            if sab_d.get("target_id") == target_id:
                same_target_count += 1

        is_reveal = same_target_count >= 2

        revenge_types = ["discount", "free", "damage"]
        revenge = {
            "type": random.choice(revenge_types),
            "expires_at": now + 3600 * 6,  # 6 hours
            "saboteur_id": actor_id,
        }

        # Attacker block-advance reward: scaled by victim's depth tier so
        # raiding deeper diggers is more rewarding. Pure positive incentive
        # to actually use sabotage — attacker still pays JC cost.
        if target_depth < 100:
            attacker_block_reward = 3
        elif target_depth < 250:
            attacker_block_reward = 5
        else:
            attacker_block_reward = 7

        self.dig_repo.atomic_sabotage(
            actor_id=actor_id,
            target_id=target_id,
            guild_id=guild_id,
            target_depth_delta=-damage,
            actor_jc_cost=cost,
            actor_depth_delta=attacker_block_reward,
            revenge={
                "target": actor_id,
                "type": revenge["type"],
                "until": revenge["expires_at"],
            },
            log_detail={
                "target_id": target_id, "damage": damage, "cost": cost,
                "trap_triggered": False,
                "attacker_block_reward": attacker_block_reward,
            },
        )

        # Mana: Black attackers also skim a slice of the victim's depth as
        # a JC bonus (steal_depth_pct). Settled separately so the audit log
        # already captured the base damage.
        attacker_steal_jc = 0
        if self.mana_effects_service is not None:
            try:
                _sab_mod = self.mana_effects_service.apply_sabotage_modifiers(
                    actor_id, guild_id, base_cost=cost,
                )
                steal_pct = _sab_mod.get("steal_depth_pct", 0.0)
                if steal_pct > 0 and target_depth > 0:
                    attacker_steal_jc = max(1, int(target_depth * steal_pct * 0.5))
                    self.player_repo.add_balance(
                        actor_id, guild_id, attacker_steal_jc,
                    )
            except Exception:
                logger.debug("Black sabotage steal failed", exc_info=True)

        # Relic: Vendetta Coin — when the *target* has it, reflect 50% of
        # damage back at the attacker as JC pain + grant target a small JC
        # bonus. Logged with action_type="vendetta_reflect" (defensive event
        # owned by the target) so /wrapped doesn't misattribute the target as
        # a saboteur on (action_type, discord_id) joins.
        vendetta_reflect = 0
        vendetta_bonus = 0
        if self._has_relic(target_id, guild_id, "vendetta_coin"):
            vendetta_reflect = max(1, int(damage * 0.5))
            vendetta_bonus = 5
            try:
                self.player_repo.add_balance(actor_id, guild_id, -vendetta_reflect)
                self.player_repo.add_balance(target_id, guild_id, vendetta_bonus)
                self.dig_repo.log_action(
                    discord_id=target_id, guild_id=guild_id,
                    action_type="vendetta_reflect",
                    details=json.dumps({
                        "attacker_id": actor_id,
                        "reflected": vendetta_reflect,
                        "target_bonus": vendetta_bonus,
                    }),
                )
            except Exception:
                logger.warning("Vendetta Coin reflect failed", exc_info=True)
                vendetta_reflect = 0
                vendetta_bonus = 0

        return self._ok(
            cost=cost,
            damage=damage,
            target_tunnel=target_tunnel.get("tunnel_name", "Unknown Tunnel"),
            trap_triggered=False,
            trap_detail=None,
            clue=clue,
            is_reveal=is_reveal,
            insurance_applied=total_reduction > 0,
            damage_reduced=total_reduction > 0,
            mana_steal_jc=attacker_steal_jc,
            attacker_block_reward=attacker_block_reward,
            vendetta_reflect=vendetta_reflect,
            vendetta_bonus=vendetta_bonus,
        )

    def _generate_clue(self, actor_id: int, guild_id, clue_type: str) -> dict:
        """Generate a clue about the saboteur."""
        actor_tunnel = self.dig_repo.get_tunnel(actor_id, guild_id)
        if clue_type == "first_letter":
            # Use tunnel name first letter
            name = actor_tunnel.get("tunnel_name", "?") if actor_tunnel else "?"
            return {"type": "first_letter", "hint": f"Saboteur's tunnel starts with '{name[0]}'"}
        elif clue_type == "depth_range":
            depth = actor_tunnel.get("depth", 0) if actor_tunnel else 0
            low = (depth // 10) * 10
            high = low + 10
            return {"type": "depth_range", "hint": f"Saboteur is between depth {low}-{high}"}
        elif clue_type == "pickaxe_tier":
            tier = (
                self._get_active_pickaxe_tier(actor_id, guild_id, dict(actor_tunnel))
                if actor_tunnel else 0
            )
            tier_name = PICKAXE_TIERS[tier]["name"] if tier < len(PICKAXE_TIERS) else "Basic"
            return {"type": "pickaxe_tier", "hint": f"Saboteur uses a {tier_name} pickaxe"}
        return {"type": "unknown", "hint": "No clue available."}

    # ------------------------------------------------------------------
    # Tunnel Info
    # ------------------------------------------------------------------

    def get_tunnel_info(self, discord_id: int, guild_id) -> dict | None:
        """
        Get comprehensive tunnel info for a player.

        Returns None if no tunnel exists.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return None

        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id

        # Apply lazy decay
        decay_info = self._apply_lazy_decay(tunnel, guild_id)

        # Gather data
        inventory = self.get_inventory(discord_id, guild_id)
        relics = self._get_equipped_relics_for_player(discord_id, guild_id)
        recent_helpers = self.dig_repo.get_recent_actions(
            discord_id, guild_id, action_type="help", hours=24
        )
        recent_events = self.dig_repo.get_recent_actions(
            discord_id, guild_id, action_type=None, hours=168
        )

        depth = tunnel.get("depth", 0)
        layer = self._get_layer(depth)
        boss_progress = self._get_boss_progress(tunnel)
        next_boss = self._next_boss_boundary(depth, boss_progress)
        at_boss = self._at_boss_boundary(depth, boss_progress)
        queued = self._get_queued_items_for_tunnel(discord_id, guild_id)

        # Next milestone
        next_milestone = None
        for m_depth in sorted(MILESTONES.keys()):
            if depth < m_depth:
                next_milestone = {"depth": m_depth, "reward": MILESTONES[m_depth]}
                break

        cooldown = self._get_cooldown_remaining(tunnel)

        # Surface a subtle pinnacle foreshadow line once all tier bosses
        # are cleared but the pinnacle itself is still standing. Hidden when
        # not eligible.
        pinnacle_foreshadow = self._pinnacle_foreshadow_line(tunnel)

        return {
            "tunnel": tunnel,
            "depth": depth,
            "layer": layer,
            "inventory": inventory,
            "relics": relics,
            "recent_helpers": recent_helpers[:5],
            "recent_events": recent_events[:5],
            "next_milestone": next_milestone,
            "boss_progress": boss_progress,
            "next_boss": next_boss,
            "at_boss": at_boss,
            "queued_items": queued,
            "cooldown_remaining": cooldown,
            "decay_info": decay_info,
            "prestige_level": tunnel.get("prestige_level", 0) or 0,
            "streak": tunnel.get("streak_days", 0) or 0,
            "pinnacle_foreshadow": pinnacle_foreshadow,
        }

    # ------------------------------------------------------------------
    # Leaderboard
    # ------------------------------------------------------------------

    def get_leaderboard(self, guild_id) -> dict:
        """Get top 10 tunnels and ASCII community mine view."""
        return self.leaderboard_service.get_leaderboard(guild_id)

    # ------------------------------------------------------------------
    # Boss Methods
    # ------------------------------------------------------------------

    def get_flex_data(self, discord_id: int, guild_id) -> dict:
        """Return tunnel info, titles, prestige emoji, stats."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("No tunnel found.")

        tunnel = dict(tunnel)

        boss_progress = self._get_boss_progress(tunnel)
        all_bosses_beaten = all(
            (v.get("status") if isinstance(v, dict) else v) == "defeated"
            for v in boss_progress.values()
        )

        titles = []
        if all_bosses_beaten:
            titles.append("Boss Slayer")

        prestige_level = tunnel.get("prestige_level", 0) or 0
        prestige_emoji = ["", "⭐", "⭐⭐", "⭐⭐⭐", "⭐⭐⭐⭐", "⭐⭐⭐⭐⭐"]
        p_emoji = prestige_emoji[min(prestige_level, len(prestige_emoji) - 1)]

        return self._ok(
            tunnel_name=tunnel.get("tunnel_name", "Unknown"),
            depth=tunnel.get("depth", 0),
            total_digs=tunnel.get("total_digs", 0),
            total_jc_earned=tunnel.get("total_jc_earned", 0),
            prestige_level=prestige_level,
            prestige_emoji=p_emoji,
            titles=titles,
            streak=tunnel.get("streak_days", 0) or 0,
            layer=self._get_layer(tunnel.get("depth", 0)).get("name", "dirt"),
        )

    def get_guild_stats(self, guild_id) -> dict:
        """Aggregate stats for the guild."""
        return self.leaderboard_service.get_guild_stats(guild_id)

