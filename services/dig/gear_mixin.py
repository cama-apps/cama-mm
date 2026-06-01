"""GearMixin mixin for :class:`DigService`.

Gear and relic loadouts, the gear shop, repairs, drops,
consumables, defenses, and artifacts.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import random
import time

from domain.models.dig_gear import GearLoadout, GearPiece, GearSlot
from services.dig._common import (
    logger,
)
from services.dig_constants import (
    ARTIFACT_POOL,
    GEAR_BOSS_DROP_RATE,
    GEAR_DROP_DEPTH_TIER_MAP,
    GEAR_MAX_DURABILITY,
    GEAR_REPAIR_COST_PCT,
    GEAR_TIER_TABLES,
    PLAYER_HIT_CEILING,
    PLAYER_HIT_FLOOR,
    RELIC_SLOTS_BASE,
    RELIC_SLOTS_MAX,
    TROPHY_CARVE_RATE,
    TROPHY_RELIC_IDS,
    format_relic_label,
)


class GearMixin:
    """GearMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """
    def _relic_jc_yield_multiplier(
        self,
        discord_id: int,
        guild_id,
        *,
        weather_code: str | None = None,
        luminosity: int | None = None,
        is_first_dig_today: bool = False,
        include_random: bool = True,
    ) -> float:
        """Combined JC-yield multiplier from yield-affecting relics.

        - Echo Lantern: ×1.15
        - Bloodstone: 50/50 ×1.5 or ×0.75 (only when ``include_random`` is True
          — preview paths pass False so the range stays representative)
        - Stormcaller: storm weather ×1.5, sunny ×1.10, else ×1.0
        - Deepveined Coal: ×1.20 while in the dark (Dark / Pitch-Black luminosity)
        - Midas Splinter: ~4% chance ×2 (random proc, gated by ``include_random``)
        - Lucky Seam: ~0.5% chance ×10 (rare jackpot, gated by ``include_random``)
        - First Light: ×2 on the day's first dig (``is_first_dig_today``)
        """
        mult = 1.0
        if self._has_relic(discord_id, guild_id, "echo_lantern"):
            mult *= 1.15
        if include_random and self._has_relic(discord_id, guild_id, "bloodstone"):
            mult *= 1.5 if random.random() < 0.5 else 0.75
        if self._has_relic(discord_id, guild_id, "stormcaller"):
            w = (weather_code or "").lower()
            if w == "storm":
                mult *= 1.5
            elif w == "sunny":
                mult *= 1.10
        if (
            luminosity is not None
            and self._get_luminosity_level(luminosity) in ("dark", "pitch_black")
            and self._has_relic(discord_id, guild_id, "deepveined_coal")
        ):
            mult *= 1.20
        if (
            include_random
            and self._has_relic(discord_id, guild_id, "midas_splinter")
            and random.random() < 0.04
        ):
            mult *= 2.0
        if (
            include_random
            and self._has_relic(discord_id, guild_id, "lucky_seam")
            and random.random() < 0.005
        ):
            mult *= 10.0
        if is_first_dig_today and self._has_relic(discord_id, guild_id, "first_light"):
            mult *= 2.0
        return mult

    def _is_first_dig_of_day(self, last_dig_at, today: str) -> bool:
        """True if the player's previous dig was on an earlier game-day.

        Drives the First Light relic (first-dig-of-day ×2 yield). A tunnel with
        no prior dig counts as the first dig of the day.
        """
        if not last_dig_at:
            return True
        import datetime

        from utils.game_date import game_date_for
        prev = game_date_for(
            datetime.datetime.fromtimestamp(int(last_dig_at), tz=datetime.UTC)
        )
        return prev != today

    def _relic_storm_negates_hazard(
        self, discord_id: int, guild_id, weather_code: str | None
    ) -> bool:
        """Stormcaller cancels the hazard penalty during storm weather."""
        if not weather_code or weather_code.lower() != "storm":
            return False
        return self._has_relic(discord_id, guild_id, "stormcaller")

    def _prism_heart_bonuses(
        self, discord_id: int, guild_id
    ) -> dict:
        """Prism Heart relic — color-dispatched bonuses.

        Returns flat bonuses applied at advance / JC / luminosity hook sites
        when the player has Prism Heart equipped AND active mana. Defaults
        (no relic / no mana / tapped mana) zero everything out.
        """
        zero = {"advance": 0, "jc_flat": 0, "lum_recovery": 0, "siphon_chance": 0.0}
        if not self._has_relic(discord_id, guild_id, "prism_heart"):
            return zero
        effects = self._mana_effects_or_none(discord_id, guild_id)
        if effects is None:
            return zero
        if effects.color == "Red":
            return {"advance": 1, "jc_flat": 0, "lum_recovery": 0, "siphon_chance": 0.0}
        if effects.color == "Blue":
            return {"advance": 0, "jc_flat": 5, "lum_recovery": 0, "siphon_chance": 0.0}
        if effects.color == "Green":
            return {"advance": 0, "jc_flat": 0, "lum_recovery": 1, "siphon_chance": 0.0}
        if effects.color == "White":
            return {"advance": 1, "jc_flat": 5, "lum_recovery": 0, "siphon_chance": 0.0}
        if effects.color == "Black":
            return {"advance": 0, "jc_flat": 0, "lum_recovery": 0, "siphon_chance": 0.05}
        return zero

    def _claim_slow_drip(
        self, discord_id: int, guild_id, *, last_dig_at: int | None
    ) -> int:
        """Slow Drip relic — credit lazy idle JC since the player's last dig.

        Pays 0.5 JC per minute idle (capped at 100 JC/day). Returns the JC
        credited (0 if relic not equipped, repo not wired, or cap hit).
        """
        if self.slow_drip_repo is None:
            return 0
        if not self._has_relic(discord_id, guild_id, "slow_drip"):
            return 0
        try:
            today = self._get_game_date()
            now = int(time.time())
            state = self.slow_drip_repo.get_today(discord_id, guild_id, today)
            already = int(state.get("claimed_today", 0) or 0)
            if already >= 100:
                self.slow_drip_repo.stamp_seen(discord_id, guild_id, today)
                return 0
            anchor = int(state.get("last_claim_at", 0) or 0)
            if anchor == 0:
                anchor = int(last_dig_at or 0)
            if anchor == 0 or anchor >= now:
                # No prior anchor — start the clock without crediting
                self.slow_drip_repo.stamp_seen(discord_id, guild_id, today)
                return 0
            elapsed_min = (now - anchor) // 60
            if elapsed_min <= 0:
                return 0
            cap_remaining = 100 - already
            credit = min(int(elapsed_min // 2), cap_remaining)  # 0.5/min ≈ 1 per 2 min
            if credit <= 0:
                return 0
            # Record the claim before crediting: if the credit write fails, the
            # daily cap is already consumed (player just misses out) rather than
            # leaving the claim unrecorded and re-claimable.
            self.slow_drip_repo.add_claim(discord_id, guild_id, today, credit)
            self.player_repo.add_balance(discord_id, guild_id, credit)
            return credit
        except Exception:
            logger.debug("Slow Drip claim failed", exc_info=True)
            return 0


    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _get_equipped_relics_for_player(self, discord_id: int, guild_id) -> list[dict]:
        """Get list of equipped relic artifacts from DB."""
        return self.dig_repo.get_equipped_relics(discord_id, guild_id)

    def _equipped_relic_ids(self, discord_id: int, guild_id) -> frozenset[str]:
        """Return frozenset of equipped relic IDs, cached per (player, guild)."""
        key = (int(discord_id), self.player_repo.normalize_guild_id(guild_id))
        cached = self._relic_cache.get(key)
        if cached is not None:
            return cached
        relics = self._get_equipped_relics_for_player(discord_id, guild_id)
        ids = frozenset(
            r.get("artifact_id") for r in relics if r.get("artifact_id")
        )
        self._relic_cache[key] = ids
        # Soft cap to prevent unbounded growth in long-lived bot processes.
        if len(self._relic_cache) > 256:
            self._relic_cache.pop(next(iter(self._relic_cache)))
        return ids

    def _invalidate_relic_cache(self, discord_id: int, guild_id) -> None:
        """Drop the cached relic-id set for a (player, guild). Called after
        equip / unequip mutations."""
        key = (int(discord_id), self.player_repo.normalize_guild_id(guild_id))
        self._relic_cache.pop(key, None)

    def _has_relic(self, discord_id: int, guild_id, relic_id: str) -> bool:
        """Check if a specific relic is equipped."""
        return relic_id in self._equipped_relic_ids(discord_id, guild_id)

    # ── Boss-combat Gear ─────────────────────────────────────────────

    def _hydrate_gear_piece(self, row: dict) -> GearPiece | None:
        """Build a GearPiece (with its tier_def attached) from a dig_gear row."""
        if row is None:
            return None
        try:
            slot = GearSlot(row["slot"])
        except ValueError:
            return None
        table = GEAR_TIER_TABLES.get(slot, [])
        tier_idx = int(row["tier"])
        if tier_idx < 0 or tier_idx >= len(table):
            return None
        return GearPiece(
            id=int(row["id"]),
            slot=slot,
            tier=tier_idx,
            durability=int(row["durability"]),
            equipped=bool(row["equipped"]),
            acquired_at=int(row["acquired_at"]),
            source=str(row.get("source") or "shop"),
            tier_def=table[tier_idx],
        )

    def _get_loadout(self, discord_id: int, guild_id) -> GearLoadout:
        """Bundle a player's four equipped gear slots + their relics."""
        equipped = self.dig_repo.get_equipped_gear(discord_id, guild_id)
        return GearLoadout(
            weapon=self._hydrate_gear_piece(equipped.get("weapon")),
            armor=self._hydrate_gear_piece(equipped.get("armor")),
            boots=self._hydrate_gear_piece(equipped.get("boots")),
            amulet=self._hydrate_gear_piece(equipped.get("amulet")),
            relics=self._get_equipped_relics_for_player(discord_id, guild_id),
        )

    def _apply_gear_to_combat(self, base: dict, loadout: GearLoadout) -> dict:
        """Fold a loadout's combat modifiers into the base BOSS_DUEL_STATS dict.

        Returns a new dict with player_hp / player_hit / player_dmg / boss_hit
        / boss_dmg adjusted, and any other keys passed through unchanged
        (e.g. ``boss_hp``). ``player_hit`` is clamped to the same floor and
        ceiling that ``fight_boss`` already enforces; ``boss_hit`` is floored
        at 0.05 to keep at least some danger.
        """
        mods = loadout.combat_modifiers()
        player_hit = base["player_hit"] + mods["player_hit"]
        player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))
        boss_hit = max(0.05, base["boss_hit"] - mods["boss_hit_reduction"])
        out = dict(base)
        out["player_hp"] = int(base["player_hp"]) + int(mods["player_hp_bonus"])
        out["player_hit"] = player_hit
        out["player_dmg"] = int(base["player_dmg"]) + int(mods["player_dmg"])
        out["boss_hit"] = boss_hit
        out["boss_dmg"] = int(base["boss_dmg"])
        # Amulet crit stats stack additively on the risk-tier baseline.
        out["crit_chance"] = float(base.get("crit_chance", 0) or 0) + float(mods["crit_chance"])
        out["crit_bonus"] = int(base.get("crit_bonus", 0) or 0) + int(mods["crit_bonus"])
        return out

    def _get_active_pickaxe_tier(self, discord_id: int, guild_id, tunnel: dict) -> int:
        """Tier index used by dig-flow code.

        Reads the equipped Weapon row first; falls back to the legacy
        ``tunnels.pickaxe_tier`` column when no weapon is equipped (covers
        tests, brand-new tunnels, and the rare case of a player
        unequipping their only pickaxe).
        """
        equipped = self.dig_repo.get_equipped_gear(discord_id, guild_id)
        wpn = equipped.get("weapon")
        if wpn is not None:
            return int(wpn["tier"])
        return int(tunnel.get("pickaxe_tier", 0) or 0)

    def get_loadout(self, discord_id: int, guild_id) -> dict:
        """Public serialization of the equipped loadout for the /dig gear panel."""
        loadout = self._get_loadout(discord_id, guild_id)
        def serialize(p: GearPiece | None) -> dict | None:
            if p is None:
                return None
            return {
                "id": p.id,
                "slot": p.slot.value,
                "tier": p.tier,
                "name": p.tier_def.name,
                "durability": p.durability,
                "max_durability": GEAR_MAX_DURABILITY,
                "equipped": p.equipped,
            }
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        prestige = int(tunnel.get("prestige_level", 0) or 0) if tunnel else 0
        return {
            "weapon": serialize(loadout.weapon),
            "armor":  serialize(loadout.armor),
            "boots":  serialize(loadout.boots),
            "amulet": serialize(loadout.amulet),
            "relics": list(loadout.relics),
            "relic_cap": self._relic_slot_cap(prestige),
        }

    def pop_relic_trim_notice(self, discord_id: int, guild_id) -> bool:
        """One-shot: return True (and clear the flag) if this player's relic
        loadout was trimmed by the cap rollout and they haven't been told yet."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if not tunnel or int(tunnel.get("relic_trim_notice", 0) or 0) != 1:
            return False
        self.dig_repo.update_tunnel(discord_id, guild_id, relic_trim_notice=0)
        return True

    def get_inventory_gear(self, discord_id: int, guild_id) -> list[dict]:
        """All gear pieces a player owns (any slot, equipped or not)."""
        rows = self.dig_repo.get_gear(discord_id, guild_id)
        out = []
        for row in rows:
            piece = self._hydrate_gear_piece(row)
            if piece is None:
                continue
            out.append({
                "id": piece.id,
                "slot": piece.slot.value,
                "tier": piece.tier,
                "name": piece.tier_def.name,
                "durability": piece.durability,
                "max_durability": GEAR_MAX_DURABILITY,
                "equipped": piece.equipped,
            })
        return out

    def equip_gear(self, discord_id: int, guild_id, gear_id: int) -> dict:
        """Equip a gear piece. Refuses if broken or not owned by this player."""
        row = self.dig_repo.get_gear_by_id(gear_id)
        if row is None:
            return self._error("That gear piece doesn't exist.")
        gid = self.dig_repo.normalize_guild_id(guild_id)
        if int(row["discord_id"]) != int(discord_id) or int(row["guild_id"]) != gid:
            return self._error("That gear piece doesn't belong to you.")
        if int(row["durability"]) <= 0:
            return self._error("That piece is broken — repair it first.")
        if int(row["equipped"]) == 1:
            return self._error("That piece is already equipped.")
        self.dig_repo.equip_gear(gear_id, discord_id, guild_id, row["slot"])
        return self._ok(slot=row["slot"], gear_id=gear_id)

    def unequip_gear(self, discord_id: int, guild_id, gear_id: int) -> dict:
        """Unequip a gear piece by id (no-op if already unequipped)."""
        row = self.dig_repo.get_gear_by_id(gear_id)
        if row is None:
            return self._error("That gear piece doesn't exist.")
        gid = self.dig_repo.normalize_guild_id(guild_id)
        if int(row["discord_id"]) != int(discord_id) or int(row["guild_id"]) != gid:
            return self._error("That gear piece doesn't belong to you.")
        self.dig_repo.unequip_gear(gear_id)
        return self._ok(slot=row["slot"], gear_id=gear_id)

    def _gear_repair_cost(self, slot: str, tier: int) -> int:
        """Repair price = ``GEAR_REPAIR_COST_PCT`` of the tier's shop_price."""
        try:
            slot_enum = GearSlot(slot)
        except ValueError:
            return 0
        table = GEAR_TIER_TABLES.get(slot_enum, [])
        if tier < 0 or tier >= len(table):
            return 0
        return int(round(table[tier].shop_price * GEAR_REPAIR_COST_PCT))

    def compute_repair_cost(self, slot: str, tier: int) -> int:
        """Public read of the repair price for a (slot, tier). Mirrors the
        cost ``repair_gear`` would charge for a damaged piece, without
        touching balance or durability."""
        return self._gear_repair_cost(slot, tier)

    def compute_repair_all_cost(self, discord_id: int, guild_id) -> int:
        """Sum repair cost across every damaged piece the player owns."""
        rows = self.dig_repo.get_gear(discord_id, guild_id)
        return sum(
            self._gear_repair_cost(r["slot"], int(r["tier"]))
            for r in rows
            if int(r["durability"]) < GEAR_MAX_DURABILITY
        )

    def repair_gear(self, discord_id: int, guild_id, gear_id: int) -> dict:
        """Restore one piece to full durability for a JC cost.

        Uses ``player_repo.try_debit`` so the balance check and the JC
        debit happen as one atomic statement — a concurrent fight wager
        cannot race the check and drive the balance negative.
        """
        row = self.dig_repo.get_gear_by_id(gear_id)
        if row is None:
            return self._error("That gear piece doesn't exist.")
        gid = self.dig_repo.normalize_guild_id(guild_id)
        if int(row["discord_id"]) != int(discord_id) or int(row["guild_id"]) != gid:
            return self._error("That gear piece doesn't belong to you.")
        if int(row["durability"]) >= GEAR_MAX_DURABILITY:
            return self._error("That piece is already at full durability.")
        cost = self._gear_repair_cost(row["slot"], int(row["tier"]))
        if cost > 0 and not self.player_repo.try_debit(discord_id, guild_id, cost):
            balance = self.player_repo.get_balance(discord_id, guild_id)
            return self._error(f"Repair costs {cost} JC; you only have {balance}.")
        self.dig_repo.repair_gear(gear_id, GEAR_MAX_DURABILITY)
        return self._ok(gear_id=gear_id, cost=cost)

    def repair_all_gear(self, discord_id: int, guild_id) -> dict:
        """Repair every owned damaged piece in one billing transaction.

        Total cost is debited atomically via ``try_debit``; on insufficient
        balance no repair runs and no JC is deducted.
        """
        rows = self.dig_repo.get_gear(discord_id, guild_id)
        damaged = [
            r for r in rows
            if int(r["durability"]) < GEAR_MAX_DURABILITY
        ]
        if not damaged:
            return self._error("Nothing to repair.")
        total_cost = sum(self._gear_repair_cost(r["slot"], int(r["tier"])) for r in damaged)
        if total_cost > 0 and not self.player_repo.try_debit(discord_id, guild_id, total_cost):
            balance = self.player_repo.get_balance(discord_id, guild_id)
            return self._error(
                f"Total repair costs {total_cost} JC; you only have {balance}.",
            )
        for r in damaged:
            self.dig_repo.repair_gear(int(r["id"]), GEAR_MAX_DURABILITY)
        return self._ok(repaired=len(damaged), cost=total_cost)

    def buy_gear(self, discord_id: int, guild_id, slot: str, tier: int) -> dict:
        """Buy a gear piece from the shop. Enforces depth/prestige/JC gates."""
        try:
            slot_enum = GearSlot(slot)
        except ValueError:
            return self._error("Invalid gear slot.")
        table = GEAR_TIER_TABLES.get(slot_enum, [])
        if tier < 0 or tier >= len(table):
            return self._error("Invalid gear tier.")
        td = table[tier]
        # Top tiers (Obsidian+) are drop-only; shop carries Wooden..Diamond (0..3).
        if tier > 3:
            return self._error(f"{td.name} doesn't drop in the shop — it comes from boss kills.")
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")
        depth = int(tunnel.get("depth", 0) or 0)
        prestige = int(tunnel.get("prestige_level", 0) or 0)
        if depth < td.depth_required:
            return self._error(f"{td.name} requires depth {td.depth_required}.")
        if prestige < td.prestige_required:
            return self._error(f"{td.name} requires prestige {td.prestige_required}.")
        if td.shop_price > 0 and not self.player_repo.try_debit(
            discord_id, guild_id, td.shop_price,
        ):
            balance = self.player_repo.get_balance(discord_id, guild_id)
            return self._error(
                f"{td.name} costs {td.shop_price} JC; you have {balance}.",
            )
        gear_id = self.dig_repo.add_gear(
            discord_id, guild_id, slot_enum.value, tier, source="shop",
        )
        return self._ok(gear_id=gear_id, name=td.name, cost=td.shop_price)

    def _relic_slot_cap(self, prestige: int) -> int:
        """Equippable relic slots for a prestige level, bounded by the ceiling."""
        return min(int(prestige) + RELIC_SLOTS_BASE, RELIC_SLOTS_MAX)

    def equip_relic_for_player(self, discord_id: int, guild_id, artifact_db_id: int) -> dict:
        """Equip a relic, enforcing the prestige-scaled cap.

        The cap is ``min(prestige_level + RELIC_SLOTS_BASE, RELIC_SLOTS_MAX)``.
        Equipping over the cap is rejected — caller (the panel) is expected to
        ask the user to unequip something first.
        """
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        target = next((a for a in artifacts if int(a["id"]) == int(artifact_db_id)), None)
        if target is None:
            return self._error("That relic isn't in your inventory.")
        if int(target.get("is_relic", 0)) != 1:
            return self._error("That artifact isn't a relic and can't be equipped.")
        if int(target.get("equipped", 0)) == 1:
            return self._error("That relic is already equipped.")
        # Relics are unique — refuse to equip a second copy of the same relic
        # (guards against duplicate rows left over from before relics were unique).
        target_aid = target.get("artifact_id")
        if any(
            a.get("artifact_id") == target_aid and int(a.get("equipped", 0)) == 1
            for a in artifacts
        ):
            return self._error("You already have that relic equipped.")
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        prestige = int(tunnel.get("prestige_level", 0) or 0) if tunnel else 0
        cap = self._relic_slot_cap(prestige)
        equipped_count = self.dig_repo.count_equipped_relics(discord_id, guild_id)
        if equipped_count >= cap:
            return self._error(
                f"You've hit your relic cap ({cap}). Unequip one first.",
            )
        self.dig_repo.equip_relic(int(artifact_db_id), True)
        self._invalidate_relic_cache(discord_id, guild_id)
        return self._ok(artifact_id=target.get("artifact_id"), cap=cap)

    def unequip_relic_for_player(self, discord_id: int, guild_id, artifact_db_id: int) -> dict:
        """Unequip a relic owned by this player."""
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        target = next((a for a in artifacts if int(a["id"]) == int(artifact_db_id)), None)
        if target is None:
            return self._error("That relic isn't in your inventory.")
        self.dig_repo.unequip_relic(int(artifact_db_id))
        self._invalidate_relic_cache(discord_id, guild_id)
        return self._ok(artifact_id=target.get("artifact_id"))

    def _maybe_drop_gear(self, discord_id: int, guild_id, at_boss: int) -> dict | None:
        """Roll a single boss-drop after a kill. Returns the drop payload or None."""
        if at_boss not in GEAR_DROP_DEPTH_TIER_MAP:
            return None
        if random.random() >= GEAR_BOSS_DROP_RATE:
            return None
        tier = GEAR_DROP_DEPTH_TIER_MAP[at_boss]
        slot_choice = random.choice(["weapon", "armor", "boots", "amulet"])
        gear_id = self.dig_repo.add_gear(
            discord_id, guild_id, slot_choice, tier, source="boss_drop",
        )
        try:
            slot_enum = GearSlot(slot_choice)
            name = GEAR_TIER_TABLES[slot_enum][tier].name
        except (ValueError, KeyError, IndexError):
            name = f"{slot_choice} (tier {tier})"
        return {"gear_id": gear_id, "slot": slot_choice, "tier": tier, "name": name}

    # Probability that a prestige-gated relic drops on a boss kill,
    # gated by ``min_prestige`` per relic.
    _PRESTIGE_RELIC_DROP_RATE: float = 0.10

    def _maybe_drop_prestige_relic(
        self, discord_id: int, guild_id, prestige_level: int,
    ) -> dict | None:
        """Roll a prestige-gated relic on a boss kill.

        Filters ``RELICS`` by ``min_prestige <= prestige_level`` and
        considers only entries with ``min_prestige > 0`` (the new pool).
        Uses the working ``add_artifact(... is_relic=True)`` signature
        — never goes through the broken ``roll_artifact`` path.
        """
        from services.dig_constants import RELICS

        # Relics are unique — don't drop one the player already owns.
        owned = {
            dict(a).get("artifact_id")
            for a in (self.dig_repo.get_artifacts(discord_id, guild_id) or [])
        }
        eligible = [
            r for r in RELICS
            if r.min_prestige > 0 and r.min_prestige <= prestige_level
            and r.id not in TROPHY_RELIC_IDS
            and r.id not in owned
        ]
        if not eligible:
            return None
        if random.random() >= self._PRESTIGE_RELIC_DROP_RATE:
            return None
        choice = random.choice(eligible)
        self.dig_repo.add_artifact(discord_id, guild_id, choice.id, is_relic=True)
        return {"id": choice.id, "name": choice.name, "rarity": choice.rarity}

    def _maybe_carve_trophy_relic(self, discord_id: int, guild_id, boss_def) -> dict | None:
        """Carve a boss's signature trophy relic on defeat (MH-style).

        Rolls ``TROPHY_CARVE_RATE`` per kill until the player owns the trophy;
        once owned it never re-drops (dig has no duplicate sink). Returns the
        relic descriptor on a successful carve, else ``None``.
        """
        trophy_id = getattr(boss_def, "trophy_relic_id", "") or ""
        if not trophy_id:
            return None
        owned = {
            dict(a).get("artifact_id")
            for a in (self.dig_repo.get_artifacts(discord_id, guild_id) or [])
        }
        if trophy_id in owned:
            return None
        if random.random() >= TROPHY_CARVE_RATE:
            return None
        self.dig_repo.add_artifact(discord_id, guild_id, trophy_id, is_relic=True)
        from services.dig_constants import ARTIFACT_BY_ID
        defn = ARTIFACT_BY_ID.get(trophy_id)
        return {
            "id": trophy_id,
            "name": defn.name if defn else trophy_id,
            "rarity": defn.rarity if defn else "Rare",
        }

    def _get_queued_items_for_tunnel(self, discord_id: int, guild_id) -> list[dict]:
        """Get items queued for next dig from inventory table."""
        items = self.dig_repo.get_queued_items(discord_id, guild_id)
        return [{"type": i.get("item_type"), "id": i.get("id")} for i in items]

    def get_owned_relics(self, discord_id: int, guild_id) -> list[dict]:
        """Return list of relics owned by the player.

        ``id`` is the artifact_id string (used by /dig gift autocomplete);
        ``db_id`` is the dig_artifacts.id row primary key (used by the
        gear panel to call equip/unequip).
        """
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        relics = []
        for a in (artifacts or []):
            a = dict(a)
            if a.get("is_relic"):
                artifact_id = a.get("artifact_id", "")
                relics.append({
                    "id": artifact_id,
                    "db_id": a.get("id"),
                    "name": format_relic_label(artifact_id),
                    "equipped": a.get("equipped", 0),
                })
        return relics

    def use_item(self, discord_id: int, guild_id, item_type: str) -> dict:
        """Queue an item for next dig."""
        return self.inventory_service.use_item(discord_id, guild_id, item_type)

    def queue_item(self, discord_id: int, guild_id, item_id: int) -> dict:
        """Queue a specific inventory item by its database id."""
        return self.inventory_service.queue_item(discord_id, guild_id, item_id)

    def buy_item(self, discord_id: int, guild_id, item_type: str) -> dict:
        """Buy an item from the shop."""
        return self.inventory_service.buy_item(discord_id, guild_id, item_type)

    def get_inventory(self, discord_id: int, guild_id) -> list[dict]:
        """Return inventory items with names and queued status."""
        return self.inventory_service.get_inventory(discord_id, guild_id)

    # ------------------------------------------------------------------
    # Defense
    # ------------------------------------------------------------------

    def set_trap(self, discord_id: int, guild_id) -> dict:
        """Set a trap on your tunnel."""
        return self.inventory_service.set_trap(discord_id, guild_id)

    def buy_insurance(self, discord_id: int, guild_id) -> dict:
        """Buy 24h sabotage insurance."""
        return self.inventory_service.buy_insurance(discord_id, guild_id)

    # ------------------------------------------------------------------
    # Artifacts
    # ------------------------------------------------------------------

    def roll_artifact(self, discord_id: int, guild_id, depth: int, *, extra_rate_mod: float = 1.0) -> dict | None:
        """Roll for a raw-dig relic find. Returns relic info or None.

        Only **entry-level basic relics** (min-prestige 0, Rare) are findable
        from digging, and only ones the player doesn't already own (relics are
        unique). Everything else — legendaries, prestige-gated relics, trophies
        — comes from bosses or prestige grants. Find chance is the Rare rate
        (0.5%) scaled by the same find modifiers as before.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return None

        tunnel = dict(tunnel)
        layer = self._get_layer(depth)
        layer_name = layer.get("name", "dirt")

        # Echo Stone relic bonus
        rate_mod = 1.1 if self._has_relic(discord_id, guild_id, "echo_stone") else 1.0
        # Weather / external artifact modifier
        rate_mod *= extra_rate_mod
        # P6 ascension: artifact find rate multiplier
        prestige_level = tunnel.get("prestige_level", 0) or 0
        ascension = self._get_ascension_effects(prestige_level)
        rate_mod *= ascension.get("artifact_multiplier", 1.0)
        # Mutation: treasure_sense (+25% artifact find)
        mutations = self._get_mutations(tunnel)
        mutation_fx = self._apply_mutation_effects(mutations)
        rate_mod *= (1.0 + mutation_fx.get("artifact_chance_bonus", 0))
        # Post-pinnacle decay applies to artifact rate
        rate_mod *= self._post_pinnacle_decay_factor(depth, discord_id, guild_id)

        # Single find roll at the Rare rate (scaled by the modifiers above).
        if random.random() >= 0.005 * rate_mod:
            return None

        # Findable pool: entry-level basic relics only (min-prestige 0, Rare),
        # excluding any the player already owns (relics are unique). Prefer the
        # current layer, fall back to any layer.
        owned = {
            dict(a).get("artifact_id")
            for a in (self.dig_repo.get_artifacts(discord_id, guild_id) or [])
        }

        def _pool(require_layer: bool) -> list[dict]:
            return [
                a for a in ARTIFACT_POOL
                if a.get("is_relic")
                and int(a.get("min_prestige", 0) or 0) == 0
                and (a.get("rarity") or "").lower() == "rare"
                and a.get("id") not in owned
                and (
                    not require_layer
                    or (a.get("layer") or "").lower() == layer_name.lower()
                )
            ]

        eligible = _pool(require_layer=True) or _pool(require_layer=False)
        if not eligible:
            return None

        artifact = random.choice(eligible)
        self.dig_repo.add_artifact(discord_id, guild_id, artifact["id"], is_relic=True)
        return {
            "id": artifact["id"],
            "name": artifact["name"],
            "rarity": "rare",
            "type": "relic",
            "is_relic": True,
            "description": artifact.get("lore_text", ""),
        }

    def gift_relic(self, giver_id: int, receiver_id: int, guild_id, artifact_id: str) -> dict:
        """Gift a relic artifact to another player."""
        if giver_id == receiver_id:
            return self._error("You can't gift to yourself.")

        # Check giver has it
        artifacts = self.dig_repo.get_artifacts(giver_id, guild_id)
        target_artifact = None
        for a in artifacts:
            if a.get("id") == artifact_id or a.get("artifact_id") == artifact_id:
                target_artifact = dict(a)
                break

        if target_artifact is None:
            return self._error("You don't have that artifact.")

        if not target_artifact.get("is_relic"):
            return self._error("Only relics can be gifted.")

        # Check receiver has a tunnel
        receiver_tunnel = self.dig_repo.get_tunnel(receiver_id, guild_id)
        if receiver_tunnel is None:
            return self._error("Receiver doesn't have a tunnel.")

        # Compute which of the giver's equipped rows to unequip before the
        # atomic transfer (read-only; safe outside BEGIN IMMEDIATE).
        relics = self._get_equipped_relics_for_player(giver_id, guild_id)
        unequip_ids = [
            r["id"] for r in relics
            if r.get("artifact_id") == target_artifact.get("artifact_id")
        ]

        # Remove from giver + insert on receiver + unequip giver copies all
        # commit together — no duplication or destruction mid-flight.
        self.dig_repo.atomic_gift_relic(
            giver_id=giver_id,
            receiver_id=receiver_id,
            guild_id=guild_id,
            artifact_db_id=target_artifact["id"],
            artifact_id=target_artifact["artifact_id"],
            unequip_artifact_db_ids=unequip_ids,
        )
        # Both sides may have changed equipped sets — invalidate caches.
        self._invalidate_relic_cache(giver_id, guild_id)
        self._invalidate_relic_cache(receiver_id, guild_id)

        return self._ok(
            artifact_id=artifact_id,
            artifact_name=target_artifact.get("name", "Unknown"),
        )

    def get_collection(self, discord_id: int, guild_id) -> dict:
        """Return all artifacts grouped by layer and rarity."""
        return self.leaderboard_service.get_collection(discord_id, guild_id)

    # ------------------------------------------------------------------
    # Events
    # ------------------------------------------------------------------
