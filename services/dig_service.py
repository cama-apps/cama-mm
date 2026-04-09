"""
Service for the tunnel digging minigame.

Handles all game logic: digging, cave-ins, bosses, prestige,
items, artifacts, sabotage, traps, and achievements.
"""

import datetime
import json
import logging
import random
import time

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_constants import (
    ACHIEVEMENTS,
    ARTIFACT_POOL,
    BOSS_ASCII,
    BOSS_BOUNDARIES,
    BOSS_DIALOGUE,
    BOSS_NAMES,
    BOSS_ODDS,
    BOSS_PAYOUTS,
    CONSUMABLE_ITEMS,
    DIG_TIPS,
    EVENT_POOL,
    FREE_DIG_COOLDOWN,
    INJURY_SLOW_COOLDOWN,
    ITEM_PRICES,
    LAYERS,
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK,
    LUMINOSITY_DARK_CAVE_IN_BONUS,
    LUMINOSITY_DARK_JC_MULTIPLIER,
    LUMINOSITY_DIM,
    LUMINOSITY_DIM_CAVE_IN_BONUS,
    LUMINOSITY_DIM_EVENT_MULTIPLIER,
    LUMINOSITY_DRAIN_PER_DIG,
    LUMINOSITY_MAX,
    LUMINOSITY_PITCH_CAVE_IN_BONUS,
    LUMINOSITY_PITCH_JC_MULTIPLIER,
    MAX_INVENTORY_SIZE,
    MAX_PRESTIGE,
    MILESTONES,
    PAID_DIG_COSTS,
    PICKAXE_TIERS,
    PRESTIGE_PERKS,
    STREAKS,
    TUNNEL_NAME_ADJECTIVES,
    TUNNEL_NAME_NOUNS,
    TUNNEL_NAME_SILLY,
    TUNNEL_NAME_TITLES,
)

logger = logging.getLogger("cama_bot.services.dig")


class DigService:
    """Encapsulates all tunnel digging minigame logic."""

    def __init__(self, dig_repo: DigRepository, player_repo: PlayerRepository):
        self.dig_repo = dig_repo
        self.player_repo = player_repo

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _get_game_date(self) -> str:
        """Get current game date (resets at 4 AM PST). Uses time.time() so tests can mock it."""
        now_utc = datetime.datetime.fromtimestamp(time.time(), tz=datetime.UTC)
        pst = datetime.timezone(datetime.timedelta(hours=-8))
        now_pst = now_utc.astimezone(pst)
        # Subtract 4 hours so the "day" starts at 4 AM PST
        game_time = now_pst - datetime.timedelta(hours=4)
        return game_time.strftime("%Y-%m-%d")

    def _get_cooldown_remaining(self, tunnel: dict) -> int:
        """Returns seconds remaining on free dig cooldown, 0 if ready."""
        if tunnel.get("last_dig_at") is None:
            return 0
        now = int(time.time())
        elapsed = now - tunnel["last_dig_at"]
        remaining = FREE_DIG_COOLDOWN - elapsed
        # Check for stun from injury
        injury = json.loads(tunnel["injury_state"]) if tunnel.get("injury_state") else None
        if injury and injury.get("type") == "slower_cooldown":
            remaining = INJURY_SLOW_COOLDOWN - elapsed
        return max(0, remaining)

    def _get_layer(self, depth: int) -> dict:
        """Return layer info for given depth."""
        for layer in reversed(LAYERS):
            if depth >= layer["min_depth"]:
                return layer
        return LAYERS[0]

    def get_layer(self, depth: int) -> dict:
        """Public: return layer info for given depth."""
        return self._get_layer(depth)

    def _get_prestige_perks(self, tunnel: dict) -> list[str]:
        """Get list of active prestige perks."""
        raw = tunnel.get("prestige_perks")
        if not raw:
            return []
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return []

    def _get_equipped_relics_for_player(self, discord_id: int, guild_id) -> list[dict]:
        """Get list of equipped relic artifacts from DB."""
        return self.dig_repo.get_equipped_relics(discord_id, guild_id)

    def _has_relic(self, discord_id: int, guild_id, relic_id: str) -> bool:
        """Check if a specific relic is equipped."""
        relics = self._get_equipped_relics_for_player(discord_id, guild_id)
        return any(r.get("artifact_id") == relic_id for r in relics)

    def _get_queued_items_for_tunnel(self, discord_id: int, guild_id) -> list[dict]:
        """Get items queued for next dig from inventory table."""
        items = self.dig_repo.get_queued_items(discord_id, guild_id)
        return [{"type": i.get("item_type"), "id": i.get("id")} for i in items]

    def _get_boss_progress(self, tunnel: dict) -> dict:
        """Get boss defeat state, merged with canonical boss list.

        Ensures all bosses from BOSS_BOUNDARIES are present — any missing
        keys are treated as "active" (prevents prestige with only old bosses).
        """
        canonical = {str(b): "active" for b in BOSS_BOUNDARIES}
        raw = tunnel.get("boss_progress")
        if not raw:
            return canonical
        try:
            stored = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return canonical
        # Merge: stored values override, but missing bosses stay "active"
        canonical.update(stored)
        return canonical

    def _get_cheers(self, tunnel: dict) -> list[dict]:
        """Get boss fight cheer data."""
        raw = tunnel.get("cheer_data")
        if not raw:
            return []
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return []

    def _next_boss_boundary(self, depth: int, boss_progress: dict) -> int | None:
        """Return the next undefeated boss boundary above current depth, or None."""
        for b in sorted(BOSS_BOUNDARIES):
            if depth < b and boss_progress.get(str(b)) == "active":
                return b
        return None

    def _at_boss_boundary(self, depth: int, boss_progress: dict) -> int | None:
        """Return the boss boundary if depth is exactly at one and boss is active."""
        for b in BOSS_BOUNDARIES:
            if depth == b - 1 and boss_progress.get(str(b)) == "active":
                return b
        return None

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

    def _get_luminosity(self, tunnel: dict) -> int:
        """Get current luminosity, applying daily reset if game date changed."""
        lum = tunnel.get("luminosity")
        if lum is None:
            return LUMINOSITY_MAX
        return max(0, min(LUMINOSITY_MAX, lum))

    def _get_luminosity_level(self, luminosity: int) -> str:
        """Return the luminosity threshold name."""
        if luminosity >= LUMINOSITY_BRIGHT:
            return "bright"
        if luminosity >= LUMINOSITY_DIM:
            return "dim"
        if luminosity >= LUMINOSITY_DARK:
            return "dark"
        return "pitch_black"

    def _apply_luminosity_drain(self, discord_id: int, guild_id, tunnel: dict, layer_name: str) -> dict:
        """
        Drain luminosity for this dig. Resets to 100 on new game day.

        Returns dict with luminosity_before, luminosity_after, level, drained.
        """
        today = self._get_game_date()
        last_lum_date = tunnel.get("streak_last_date")  # reuse game-date tracking
        luminosity = self._get_luminosity(tunnel)

        # Daily reset: if this is a new game day, restore to max
        if last_lum_date != today:
            luminosity = LUMINOSITY_MAX

        before = luminosity
        drain = LUMINOSITY_DRAIN_PER_DIG.get(layer_name, 0)
        luminosity = max(0, luminosity - drain)

        # Persist
        self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)
        tunnel["luminosity"] = luminosity

        return {
            "luminosity_before": before,
            "luminosity_after": luminosity,
            "level": self._get_luminosity_level(luminosity),
            "drained": drain,
        }

    def _luminosity_cave_in_bonus(self, luminosity: int) -> float:
        """Extra cave-in chance from low luminosity."""
        if luminosity >= LUMINOSITY_BRIGHT:
            return 0.0
        if luminosity >= LUMINOSITY_DIM:
            return LUMINOSITY_DIM_CAVE_IN_BONUS
        if luminosity >= LUMINOSITY_DARK:
            return LUMINOSITY_DARK_CAVE_IN_BONUS
        return LUMINOSITY_PITCH_CAVE_IN_BONUS

    def _luminosity_jc_multiplier(self, luminosity: int) -> float:
        """JC reward multiplier from low luminosity (risk = reward)."""
        if luminosity >= LUMINOSITY_DIM:
            return 1.0
        if luminosity >= LUMINOSITY_DARK:
            return LUMINOSITY_DARK_JC_MULTIPLIER
        return LUMINOSITY_PITCH_JC_MULTIPLIER

    # ------------------------------------------------------------------
    # Temp Buffs
    # ------------------------------------------------------------------

    def _get_active_buff(self, tunnel: dict) -> dict | None:
        """Get the active temp buff, or None if expired/absent."""
        raw = tunnel.get("temp_buffs")
        if not raw:
            return None
        try:
            buff = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return None
        if buff.get("digs_remaining", 0) <= 0:
            return None
        return buff

    def _apply_buff_effects(self, buff: dict | None) -> dict:
        """Extract numeric effects from an active buff. Returns effect dict."""
        if not buff:
            return {}
        return buff.get("effect", {})

    def _decrement_buff(self, discord_id: int, guild_id, tunnel: dict) -> None:
        """Decrement active buff duration by 1 dig. Clear if expired."""
        buff = self._get_active_buff(tunnel)
        if not buff:
            return
        remaining = buff.get("digs_remaining", 0) - 1
        if remaining <= 0:
            self.dig_repo.update_tunnel(discord_id, guild_id, temp_buffs=None)
        else:
            buff["digs_remaining"] = remaining
            self.dig_repo.update_tunnel(discord_id, guild_id, temp_buffs=json.dumps(buff))

    def set_temp_buff(self, discord_id: int, guild_id, buff_data: dict) -> None:
        """Set a temp buff on the tunnel (replaces any existing buff)."""
        payload = {
            "id": buff_data.get("id", "unknown"),
            "name": buff_data.get("name", "Unknown Buff"),
            "digs_remaining": buff_data.get("duration_digs", 1),
            "effect": buff_data.get("effect", {}),
        }
        self.dig_repo.update_tunnel(discord_id, guild_id, temp_buffs=json.dumps(payload))

    def _error(self, msg: str) -> dict:
        """Return a standard error result."""
        return {"success": False, "error": msg}

    def _ok(self, **kwargs) -> dict:
        """Return a standard success result."""
        result = {"success": True, "error": None}
        result.update(kwargs)
        # Add common aliases
        if "depth_after" in result and "depth" not in result:
            result["depth"] = result["depth_after"]
        return result

    # ------------------------------------------------------------------
    # Tunnel Name Generation
    # ------------------------------------------------------------------

    def generate_tunnel_name(self) -> str:
        """Random name from 3 pool types (40% adj+noun, 35% title, 25% silly)."""
        roll = random.random()
        if roll < 0.40:
            adj = random.choice(TUNNEL_NAME_ADJECTIVES)
            noun = random.choice(TUNNEL_NAME_NOUNS)
            return f"The {adj} {noun}"
        elif roll < 0.75:
            return random.choice(TUNNEL_NAME_TITLES)
        else:
            return random.choice(TUNNEL_NAME_SILLY)

    # ------------------------------------------------------------------
    # Lazy Decay
    # ------------------------------------------------------------------

    def _apply_lazy_decay(self, tunnel: dict, guild_id) -> dict:
        """
        Calculate and apply depth decay since last_dig_at.

        Returns dict with decay info (decayed: bool, amount, reason).
        """
        result = {"decayed": False, "amount": 0, "reason": None}

        last_dig = tunnel.get("last_dig_at")
        if last_dig is None:
            return result

        now = int(time.time())
        hours_since = (now - last_dig) / 3600.0

        if hours_since < 24:
            return result

        depth = tunnel.get("depth", 0)
        if depth <= 0:
            return result

        layer = self._get_layer(depth)
        base_rate = layer.get("decay_per_day", 1)

        # Accelerate if > 72h
        if hours_since > 72:
            base_rate *= 2

        days_elapsed = hours_since / 24.0

        # Check helpers in last 24h
        recent_helpers = self.dig_repo.get_recent_actions(
            tunnel["discord_id"], guild_id, action_type="help", hours=24
        )
        helper_count = len({a.get("actor_id") for a in recent_helpers if a.get("actor_id")})
        helper_multiplier = max(0.25, 1.0 - 0.5 * helper_count)

        # Check reinforcement item
        reinforced_until = tunnel.get("reinforced_until") or 0
        if now < reinforced_until:
            return result  # Skip decay entirely

        # Check Root Network relic (-25% decay) and Frozen Clock relic (-50% decay)
        relic_multiplier = 1.0
        if self._has_relic(tunnel["discord_id"], guild_id, "root_network"):
            relic_multiplier *= 0.75
        if self._has_relic(tunnel["discord_id"], guild_id, "frozen_clock"):
            relic_multiplier *= 0.50

        total_decay = int(base_rate * days_elapsed * helper_multiplier * relic_multiplier)

        if total_decay <= 0:
            return result

        # Don't decay past layer boundaries (25, 50, 75)
        boundaries = sorted(BOSS_BOUNDARIES)
        new_depth = depth - total_decay
        for b in reversed(boundaries):
            if depth >= b > new_depth:
                new_depth = b
                break

        new_depth = max(0, new_depth)
        actual_decay = depth - new_depth

        if actual_decay <= 0:
            return result

        # Update tunnel depth
        self.dig_repo.update_tunnel(
            tunnel["discord_id"], guild_id, depth=new_depth
        )
        tunnel["depth"] = new_depth

        # Log decay
        self.dig_repo.log_action(
            discord_id=tunnel["discord_id"],
            guild_id=guild_id,
            action_type="decay",
            details=json.dumps({"amount": actual_decay, "hours_since": round(hours_since, 1)}),
        )

        result["decayed"] = True
        result["amount"] = actual_decay
        result["reason"] = f"Your tunnel lost {actual_decay} blocks from neglect."
        return result

    # ------------------------------------------------------------------
    # Core Dig
    # ------------------------------------------------------------------

    def dig(self, discord_id: int, guild_id, paid: bool = False) -> dict:
        """
        Main dig action.

        Returns dict with: success, error, tunnel, depth_before, depth_after,
        advance, jc_earned, milestone_bonus, streak_bonus, cave_in, cave_in_detail,
        boss_encounter, boss_info, event, artifact, achievements, is_first_dig,
        items_used, tip.
        """
        # 0. Check player is registered
        if not self.player_repo.exists(discord_id, guild_id):
            return self._error("You need to register first. Use /player register.")

        now = int(time.time())
        today = self._get_game_date()

        # 1. Get or create tunnel
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        is_first_dig = False
        if tunnel is None:
            name = self.generate_tunnel_name()
            self.dig_repo.create_tunnel(discord_id, guild_id, name=name)
            tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
            is_first_dig = True

        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id

        # 2. Apply lazy decay
        decay_info = self._apply_lazy_decay(tunnel, guild_id)

        depth_before = tunnel.get("depth", 0)

        # 2b. If already at a boss boundary, return boss encounter without
        #     consuming cooldown or charging for paid dig.
        if not is_first_dig:
            boss_progress_early = self._get_boss_progress(tunnel)
            at_boss_early = self._at_boss_boundary(depth_before, boss_progress_early)
            if at_boss_early is not None:
                inv = self.dig_repo.get_inventory(discord_id, guild_id)
                has_lantern_early = any(i.get("item_type") == "lantern" for i in inv)
                boss_name = BOSS_NAMES.get(at_boss_early, "Unknown Boss")
                attempts = tunnel.get("boss_attempts", 0) or 0
                dialogue_list = BOSS_DIALOGUE.get(at_boss_early, ["..."])
                dialogue = dialogue_list[min(attempts, len(dialogue_list) - 1)]
                return self._ok(
                    tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                    depth_before=depth_before,
                    depth_after=depth_before,
                    advance=0,
                    jc_earned=0,
                    milestone_bonus=0,
                    streak_bonus=0,
                    cave_in=False,
                    cave_in_detail=None,
                    boss_encounter=True,
                    boss_info={
                        "boundary": at_boss_early,
                        "name": boss_name,
                        "dialogue": dialogue,
                        "ascii_art": BOSS_ASCII.get(at_boss_early, ""),
                    },
                    has_lantern=has_lantern_early,
                    event=None,
                    artifact=None,
                    achievements=[],
                    is_first_dig=False,
                    items_used=[],
                    tip="A boss blocks your path!",
                    decay_info=decay_info,
                    luminosity_info=None,
                )

        # 3. Cooldown / paid dig check
        paid_dig_cost = 0
        if not is_first_dig:
            cooldown_remaining = self._get_cooldown_remaining(tunnel)
            if cooldown_remaining > 0:
                if not paid:
                    # Calculate cost for informational purposes
                    pd = tunnel.get("paid_dig_date")
                    pc = tunnel.get("paid_digs_today") or 0
                    if pd != today:
                        pc = 0
                    ci = min(pc, len(PAID_DIG_COSTS) - 1)
                    return {
                        "success": False,
                        "error": f"Dig on cooldown ({cooldown_remaining}s remaining).",
                        "cooldown_remaining": cooldown_remaining,
                        "paid_dig_cost": PAID_DIG_COSTS[ci],
                        "paid_dig_available": True,
                    }

                # Paid dig requested
                paid_date = tunnel.get("paid_dig_date")
                paid_count = tunnel.get("paid_digs_today") or 0

                if paid_date != today:
                    paid_count = 0

                cost_index = min(paid_count, len(PAID_DIG_COSTS) - 1)
                paid_dig_cost = PAID_DIG_COSTS[cost_index]

                balance = self.player_repo.get_balance(discord_id, guild_id)
                if balance < paid_dig_cost:
                    return self._error(
                        f"Paid dig costs {paid_dig_cost} JC but you only have {balance} JC."
                    )

                # Debit for paid dig
                self.player_repo.add_balance(discord_id, guild_id, -paid_dig_cost)
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    paid_dig_date=today,
                    paid_digs_today=paid_count + 1,
                )

        # 4. First dig ever: guaranteed safe, welcome info
        if is_first_dig:
            advance = random.randint(3, 7)
            jc_earned = random.randint(1, 5)
            new_depth = depth_before + advance

            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                total_digs=(tunnel.get("total_digs", 0) or 0) + 1,
                last_dig_at=now,
                total_jc_earned=(tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
                streak_days=1,
                streak_last_date=today,
            )
            self.player_repo.add_balance(discord_id, guild_id, jc_earned)
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="dig",
                details=json.dumps({
                    "advance": advance, "jc": jc_earned, "first_dig": True,
                    "depth_before": depth_before, "depth_after": new_depth,
                }),
            )

            return self._ok(
                tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                depth_before=depth_before,
                depth_after=new_depth,
                advance=advance,
                jc_earned=jc_earned,
                milestone_bonus=0,
                streak_bonus=0,
                cave_in=False,
                cave_in_detail=None,
                boss_encounter=False,
                boss_info=None,
                has_lantern=False,
                event=None,
                artifact=None,
                achievements=[],
                is_first_dig=True,
                items_used=[],
                tip="Welcome to the mines! Use /dig again after the cooldown.",
                decay_info=decay_info,
            )

        # 5. Check injury state
        injury = None
        injury_advance_mod = 1.0
        if tunnel.get("injury_state"):
            try:
                injury = json.loads(tunnel["injury_state"])
            except (json.JSONDecodeError, TypeError):
                injury = None

        if injury and injury.get("digs_remaining", 0) > 0:
            if injury.get("type") == "reduced_advance":
                injury_advance_mod = 0.5
            injury["digs_remaining"] = injury["digs_remaining"] - 1
            if injury["digs_remaining"] <= 0:
                injury = None
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                injury_state=json.dumps(injury) if injury else None,
            )

        # 6. Get queued items and apply effects
        queued = self._get_queued_items_for_tunnel(discord_id, guild_id)
        items_used = []
        has_dynamite = False
        has_hard_hat = False
        has_lantern = False
        has_torch = False
        has_grappling_hook = False
        has_depth_charge = False

        for item in queued:
            itype = item.get("type")
            if itype == "dynamite":
                has_dynamite = True
                items_used.append("Dynamite")
            elif itype == "hard_hat":
                has_hard_hat = True
                items_used.append("Hard Hat")
            elif itype == "lantern":
                has_lantern = True
                items_used.append("Lantern")
            elif itype == "torch":
                has_torch = True
                items_used.append("Torch")
            elif itype == "grappling_hook":
                has_grappling_hook = True
                items_used.append("Grappling Hook")
            elif itype == "depth_charge":
                has_depth_charge = True
                items_used.append("Depth Charge")

        # Consume queued items from inventory
        if queued:
            for item in queued:
                self.dig_repo.remove_inventory_item(
                    discord_id, guild_id, item.get("type")
                )
            self.dig_repo.unqueue_all(discord_id, guild_id)

        # 7. Get layer info
        layer = self._get_layer(depth_before)

        # 7b. Apply luminosity drain
        layer_name = layer.get("name", "Dirt")
        lum_info = self._apply_luminosity_drain(discord_id, guild_id, tunnel, layer_name)
        luminosity = lum_info["luminosity_after"]

        # Torch restores +50 luminosity
        if has_torch:
            luminosity = min(LUMINOSITY_MAX, luminosity + 50)
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)
            lum_info["luminosity_after"] = luminosity

        # Spore Cloak relic: -50% luminosity drain
        if self._has_relic(discord_id, guild_id, "spore_cloak") and lum_info["drained"] > 0:
            restored = lum_info["drained"] // 2
            luminosity = min(LUMINOSITY_MAX, luminosity + restored)
            lum_info["drained"] -= restored
            lum_info["luminosity_after"] = luminosity
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

        # 7c. Get and apply active temp buff
        active_buff = self._get_active_buff(tunnel)
        buff_effects = self._apply_buff_effects(active_buff)
        buff_advance_bonus = buff_effects.get("advance_bonus", 0)
        buff_cavein_reduction = buff_effects.get("cave_in_reduction", 0.0)
        self._decrement_buff(discord_id, guild_id, tunnel)

        # 8. Prestige perks and relics
        perks = self._get_prestige_perks(tunnel)

        pickaxe_tier = tunnel.get("pickaxe_tier", 0) or 0
        pickaxe_data = PICKAXE_TIERS[pickaxe_tier] if pickaxe_tier < len(PICKAXE_TIERS) else {}
        pickaxe_advance_bonus = pickaxe_data.get("advance_bonus", 0)
        pickaxe_cavein_reduction = pickaxe_data.get("cave_in_reduction", 0)

        perk_cavein_reduction = 0.05 if "reinforced_walls" in perks else 0.0
        perk_advance_bonus = 0.1 if "efficient_digging" in perks else 0.0
        perk_loot_bonus = 0.15 if "keen_eye" in perks else 0.0

        # New expansion perks
        if "deep_sight" in perks and lum_info.get("drained", 0) > 0:
            # Restore 25% of what was drained (stacks with torch/spore_cloak)
            restored = max(1, lum_info["drained"] // 4)
            luminosity = min(LUMINOSITY_MAX, luminosity + restored)
            lum_info["luminosity_after"] = luminosity
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)
            tunnel["luminosity"] = luminosity

        relic_cavein_mod = 0.97 if self._has_relic(discord_id, guild_id, "crystal_compass") else 1.0
        mole_claws_bonus = 1 if self._has_relic(discord_id, guild_id, "mole_claws") else 0
        magma_heart_bonus = 1 if self._has_relic(discord_id, guild_id, "magma_heart") else 0

        # 9. Cave-in check
        hard_hat_charges = tunnel.get("hard_hat_charges", 0) or 0
        cave_in_chance = layer.get("cave_in_pct", 0.10)
        # dark_adaptation perk: dim luminosity has no cave-in penalty
        lum_cave_bonus = self._luminosity_cave_in_bonus(luminosity)
        if "dark_adaptation" in perks and luminosity >= LUMINOSITY_DIM and luminosity < LUMINOSITY_BRIGHT:
            lum_cave_bonus = 0.0
        cave_in_chance += lum_cave_bonus
        cave_in_chance -= perk_cavein_reduction
        cave_in_chance -= pickaxe_cavein_reduction
        cave_in_chance -= buff_cavein_reduction
        cave_in_chance *= relic_cavein_mod
        if has_hard_hat:
            cave_in_chance *= 0.5
        cave_in_chance = max(0.01, cave_in_chance)

        # Hard hat charges prevent cave-in entirely
        if hard_hat_charges > 0:
            cave_in = False
            self.dig_repo.update_tunnel(discord_id, guild_id, hard_hat_charges=hard_hat_charges - 1)
        else:
            cave_in = random.random() < cave_in_chance
        cave_in_detail = None

        if cave_in:
            # 10. Cave-in consequences
            block_loss = random.randint(3, 8)
            # Grappling hook prevents block loss
            if has_grappling_hook:
                block_loss = 0
            new_depth = max(0, depth_before - block_loss)

            # Random additional consequence
            consequence_roll = random.random()
            if consequence_roll < 0.3:
                # Stun: extra cooldown on next dig
                cave_in_detail = {
                    "type": "stun",
                    "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're stunned.",
                }
                injury = {"type": "slower_cooldown", "digs_remaining": 2}
                self.dig_repo.update_tunnel(
                    discord_id, guild_id, injury_state=json.dumps(injury)
                )
            elif consequence_roll < 0.6:
                # Injury: reduced advance
                cave_in_detail = {
                    "type": "injury",
                    "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're injured (reduced digging for 3 digs).",
                }
                injury = {"type": "reduced_advance", "digs_remaining": 3}
                self.dig_repo.update_tunnel(
                    discord_id, guild_id, injury_state=json.dumps(injury)
                )
            else:
                # Medical bill (capped at current balance to prevent negative)
                med_cost = random.randint(2, 6)
                balance = self.player_repo.get_balance(discord_id, guild_id)
                med_cost = min(med_cost, max(0, balance))
                if med_cost > 0:
                    self.player_repo.add_balance(discord_id, guild_id, -med_cost)
                cave_in_detail = {
                    "type": "medical_bill",
                    "block_loss": block_loss,
                    "jc_lost": med_cost,
                    "message": f"Cave-in! Lost {block_loss} blocks and paid {med_cost} JC in medical bills.",
                }

            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                total_digs=(tunnel.get("total_digs", 0) or 0) + 1,
                last_dig_at=now,
            )
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="dig",
                details=json.dumps({
                    "cave_in": True, "block_loss": block_loss,
                    "detail": cave_in_detail,
                    "depth_before": depth_before, "depth_after": new_depth,
                }),
            )

            achievements = self.check_achievements(
                discord_id, guild_id,
                {**tunnel, "depth": new_depth},
                {"action": "cave_in"},
            )

            return self._ok(
                tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                depth_before=depth_before,
                depth_after=new_depth,
                advance=0,
                jc_earned=0,
                milestone_bonus=0,
                streak_bonus=0,
                cave_in=True,
                cave_in_detail=cave_in_detail,
                boss_encounter=False,
                boss_info=None,
                has_lantern=has_lantern,
                event=None,
                artifact=None,
                achievements=achievements,
                is_first_dig=False,
                items_used=items_used,
                tip=self._pick_tip(new_depth),
                decay_info=decay_info,
                luminosity_info=lum_info,
            )

        # 11. Roll advance (no cave-in)
        base_min = layer.get("advance_min", 1)
        base_max = layer.get("advance_max", 5)
        # the_endless perk: The Hollow advance becomes 1-2 instead of 1-1
        if "the_endless" in perks and layer_name == "The Hollow" and base_max <= 1:
            base_max = 2
        advance = random.randint(base_min, base_max)

        # Apply modifiers
        advance += pickaxe_advance_bonus + mole_claws_bonus + buff_advance_bonus
        dynamite_bonus = 0
        if has_dynamite:
            dynamite_bonus = 5
            advance += dynamite_bonus
        depth_charge_bonus = 0
        if has_depth_charge:
            depth_charge_bonus = 8
            advance += depth_charge_bonus
        advance = int(advance * (1.0 + perk_advance_bonus) * injury_advance_mod)
        advance = max(1, advance)
        # Depth charge triggers mini cave-in penalty after advance
        if has_depth_charge:
            advance = max(1, advance - 3)

        # 12. Check boss boundary
        boss_progress = self._get_boss_progress(tunnel)
        next_boss = self._next_boss_boundary(depth_before, boss_progress)
        boss_encounter = False
        boss_info = None

        if next_boss is not None and depth_before + advance >= next_boss:
            # Cap advance to boundary - 1
            advance = max(0, next_boss - 1 - depth_before)
            boss_encounter = True
            boss_name = BOSS_NAMES.get(next_boss, "Unknown Boss")
            attempts = tunnel.get("boss_attempts", 0) or 0
            dialogue_list = BOSS_DIALOGUE.get(next_boss, ["..."])
            boss_info = {
                "boundary": next_boss,
                "name": boss_name,
                "dialogue": dialogue_list[min(attempts, len(dialogue_list) - 1)],
                "ascii_art": BOSS_ASCII.get(next_boss, ""),
            }

        new_depth = depth_before + advance

        # 13. Roll JC loot
        jc_min = layer.get("jc_min", 1)
        jc_max = layer.get("jc_max", 3)
        jc_earned = random.randint(jc_min, jc_max)
        jc_earned = int(jc_earned * (1.0 + perk_loot_bonus) * self._luminosity_jc_multiplier(luminosity)) + magma_heart_bonus
        jc_earned = max(1, jc_earned)

        # 14. Check milestones
        milestone_bonus = 0
        for m_depth, m_reward in MILESTONES.items():
            if depth_before < m_depth <= new_depth:
                milestone_bonus += m_reward

        jc_earned += milestone_bonus

        # 15. Update streak
        streak = tunnel.get("streak_days", 0) or 0
        streak_last = tunnel.get("streak_last_date")
        yesterday = (
            datetime.datetime.strptime(today, "%Y-%m-%d")
            - datetime.timedelta(days=1)
        ).strftime("%Y-%m-%d")

        if streak_last == yesterday:
            streak += 1
        elif streak_last == today:
            pass  # Already dug today, keep streak
        else:
            streak = 1

        streak_bonus = 0
        for threshold in sorted(STREAKS.keys(), reverse=True):
            if streak >= threshold:
                streak_bonus = STREAKS[threshold]
                break

        jc_earned += streak_bonus

        # 16. Roll for artifact
        artifact = self.roll_artifact(discord_id, guild_id, new_depth)

        # 17. Roll for random event (layer-specific rates, luminosity boosts)
        event_rates = {
            "Dirt": 0.08, "Stone": 0.08, "Crystal": 0.10, "Magma": 0.10,
            "Abyss": 0.12, "Fungal Depths": 0.15, "Frozen Core": 0.12, "The Hollow": 0.18,
        }
        event_chance = event_rates.get(layer_name, 0.10)
        # Dim luminosity increases event chance
        if luminosity < LUMINOSITY_BRIGHT:
            event_chance *= LUMINOSITY_DIM_EVENT_MULTIPLIER
        event = None
        if random.random() < event_chance:
            event = self.roll_event(new_depth, luminosity=luminosity)

        # 18. Check achievements
        total_digs = (tunnel.get("total_digs", 0) or 0) + 1
        tunnel_updated = {**tunnel, "depth": new_depth, "total_digs": total_digs, "streak_days": streak}
        achievements = self.check_achievements(
            discord_id, guild_id, tunnel_updated,
            {"action": "dig", "advance": advance, "boss_encounter": boss_encounter},
        )

        # 19. Update tunnel in DB
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=new_depth,
            total_digs=total_digs,
            last_dig_at=now,
            total_jc_earned=(tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
            streak_days=streak,
            streak_last_date=today,
        )

        # 20. Update player balance
        self.player_repo.add_balance(discord_id, guild_id, jc_earned)

        # 21. Log action
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="dig",
            details=json.dumps({
                "advance": advance, "jc": jc_earned,
                "depth_before": depth_before, "depth_after": new_depth,
                "boss_encounter": boss_encounter,
                "cave_in": False,
            }),
        )

        # 22. Return result
        return self._ok(
            tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
            depth_before=depth_before,
            depth_after=new_depth,
            advance=advance,
            jc_earned=jc_earned,
            milestone_bonus=milestone_bonus,
            streak_bonus=streak_bonus,
            cave_in=False,
            cave_in_detail=None,
            boss_encounter=boss_encounter,
            boss_info=boss_info,
            has_lantern=has_lantern,
            event=event,
            artifact=artifact,
            achievements=achievements,
            is_first_dig=False,
            items_used=items_used,
            tip=self._pick_tip(new_depth),
            decay_info=decay_info,
            luminosity_info=lum_info,
            paid_cost=paid_dig_cost if paid_dig_cost > 0 else 0,
            dynamite_bonus=dynamite_bonus,
        )

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
        """Return shop data: consumables, pickaxe upgrades, and inventory count."""
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
            current_tier = dict(tunnel).get("pickaxe_tier", 0)

        pickaxe_upgrades = []
        for i in range(current_tier + 1, len(PICKAXE_TIERS)):
            t = PICKAXE_TIERS[i]
            pickaxe_upgrades.append({
                "name": t["name"],
                "price": t["jc_cost"],
                "depth_req": t["depth_required"],
                "prestige_req": t.get("prestige_required", 0),
            })

        return self._ok(
            consumables=consumables,
            pickaxe_upgrades=pickaxe_upgrades,
            inventory_count=inv_count,
        )

    def get_upgrade_info(self, discord_id: int, guild_id) -> dict:
        """Return info about current and next pickaxe tier."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._ok(current_tier="Wooden", next_tier=None, eligible=False)

        tunnel = dict(tunnel)
        current_idx = tunnel.get("pickaxe_tier", 0)
        current_name = PICKAXE_TIERS[current_idx]["name"] if current_idx < len(PICKAXE_TIERS) else "Unknown"

        if current_idx >= len(PICKAXE_TIERS) - 1:
            return self._ok(current_tier=current_name, next_tier=None, eligible=False)

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

    def preview_abandon(self, discord_id: int, guild_id) -> dict:
        """Preview abandon refund without executing."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        depth = tunnel.get("depth", 0)

        if depth < 10:
            return self._error("Tunnel must be at least 10 blocks deep to abandon.")

        refund = int(depth * 0.1)
        return self._ok(refund=refund, current_depth=depth)

    def get_owned_relics(self, discord_id: int, guild_id) -> list[dict]:
        """Return list of relics owned by the player."""
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        relics = []
        for a in (artifacts or []):
            a = dict(a)
            if a.get("is_relic"):
                artifact_id = a.get("artifact_id", "")
                # Look up name from pool
                name = artifact_id
                for pool_item in ARTIFACT_POOL:
                    if pool_item["id"] == artifact_id:
                        name = pool_item["name"]
                        break
                relics.append({"id": artifact_id, "name": name, "equipped": a.get("equipped", 0)})
        return relics

    def upgrade_pickaxe(self, discord_id: int, guild_id) -> dict:
        """Upgrade pickaxe to next tier if requirements met."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        current_tier = tunnel.get("pickaxe_tier", 0)

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

        # Apply upgrade
        self.player_repo.add_balance(discord_id, guild_id, -cost)
        self.dig_repo.update_tunnel(discord_id, guild_id, pickaxe_tier=next_tier_idx)

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

        # Cap at boss boundary
        boss_progress = self._get_boss_progress(target_tunnel)
        next_boss = self._next_boss_boundary(target_depth, boss_progress)
        if next_boss is not None and target_depth + advance >= next_boss:
            advance = max(0, next_boss - 1 - target_depth)

        new_depth = target_depth + advance

        # Apply advance to target
        self.dig_repo.update_tunnel(target_id, guild_id, depth=new_depth)

        # Set helper cooldown
        now = int(time.time())
        if helper_tunnel:
            self.dig_repo.update_tunnel(helper_id, guild_id, last_dig_at=now)
        else:
            # Create a minimal tunnel for the helper so cooldown is tracked
            name = self.generate_tunnel_name()
            self.dig_repo.create_tunnel(helper_id, guild_id, name=name)
            self.dig_repo.update_tunnel(helper_id, guild_id, last_dig_at=now)

        # Helper earns 1 JC
        self.player_repo.add_balance(helper_id, guild_id, 1)

        # Log help action
        self.dig_repo.log_action(
            discord_id=helper_id, guild_id=guild_id,
            action_type="help",
            details=json.dumps({
                "target_id": target_id, "advance": advance,
                "target_depth_before": target_depth, "target_depth_after": new_depth,
            }),
        )

        return self._ok(
            advance=advance,
            target_tunnel=target_tunnel.get("tunnel_name", "Unknown Tunnel"),
            target_depth_after=new_depth,
            helper_cooldown_until=now + FREE_DIG_COOLDOWN,
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
        balance = self.player_repo.get_balance(actor_id, guild_id)
        if balance < cost:
            return self._error(f"Sabotage costs {cost} JC but you only have {balance} JC.")

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

        # Check for active trap
        if target_tunnel.get("trap_active"):
            # Trap triggered!
            trap_steal = cost * 2
            actor_tunnel = self.dig_repo.get_tunnel(actor_id, guild_id)
            actor_depth = actor_tunnel["depth"] if actor_tunnel else 0
            actor_loss = random.randint(3, 5)
            new_actor_depth = max(0, actor_depth - actor_loss)

            # Saboteur pays cost and loses extra
            self.player_repo.add_balance(actor_id, guild_id, -trap_steal)
            # Target gains bonus
            self.player_repo.add_balance(target_id, guild_id, cost)

            # Saboteur loses blocks
            if actor_tunnel:
                self.dig_repo.update_tunnel(actor_id, guild_id, depth=new_actor_depth)

            # Clear trap
            self.dig_repo.update_tunnel(target_id, guild_id, trap_active=0)

            self.dig_repo.log_action(
                discord_id=actor_id, guild_id=guild_id,
                action_type="sabotage",
                details=json.dumps({
                    "target_id": target_id, "trap_triggered": True,
                    "jc_lost": trap_steal, "blocks_lost": actor_loss,
                }),
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

        new_depth = max(0, target_depth - damage)

        # Apply damage
        self.dig_repo.update_tunnel(target_id, guild_id, depth=new_depth)

        # Debit actor
        self.player_repo.add_balance(actor_id, guild_id, -cost)

        # Generate clue about saboteur
        clue_types = ["first_letter", "depth_range", "pickaxe_tier"]
        clue_type = random.choice(clue_types)
        clue = self._generate_clue(actor_id, guild_id, clue_type)

        # Check for escalating reveal (2nd+ sabotage from same actor)
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

        # Set revenge window on target
        revenge_types = ["discount", "free", "damage"]
        revenge = {
            "type": random.choice(revenge_types),
            "expires_at": now + 3600 * 6,  # 6 hours
            "saboteur_id": actor_id,
        }
        self.dig_repo.update_tunnel(
            target_id, guild_id,
            revenge_target=actor_id,
            revenge_type=revenge["type"],
            revenge_until=revenge["expires_at"],
        )

        # Log action
        self.dig_repo.log_action(
            discord_id=actor_id, guild_id=guild_id,
            action_type="sabotage",
            details=json.dumps({
                "target_id": target_id, "damage": damage, "cost": cost,
                "trap_triggered": False,
            }),
        )

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
            tier = actor_tunnel.get("pickaxe_tier", 0) if actor_tunnel else 0
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
        achievements = self.dig_repo.get_achievements(discord_id, guild_id)
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

        return {
            "tunnel": tunnel,
            "depth": depth,
            "layer": layer,
            "inventory": inventory,
            "relics": relics,
            "achievements": achievements,
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
        }

    # ------------------------------------------------------------------
    # Leaderboard
    # ------------------------------------------------------------------

    def get_leaderboard(self, guild_id) -> dict:
        """Get top 10 tunnels and ASCII community mine view."""
        tunnels = self.dig_repo.get_top_tunnels(guild_id, limit=10)
        tunnels = [dict(t) for t in tunnels]

        # Generate ASCII art
        max_depth = max((t.get("depth", 0) for t in tunnels), default=1) or 1
        lines = []
        for i, t in enumerate(tunnels, 1):
            depth = t.get("depth", 0)
            bar_len = max(1, int(40 * depth / max_depth))
            bar = "█" * bar_len
            name = t.get("tunnel_name", "???")[:15]
            lines.append(f"{i:>2}. {name:<15} {bar} {depth}m")

        ascii_art = "\n".join(lines)

        return {
            "tunnels": tunnels,
            "ascii_art": ascii_art,
        }

    # ------------------------------------------------------------------
    # Boss Methods
    # ------------------------------------------------------------------

    def encounter_boss(self, discord_id: int, guild_id) -> dict:
        """Check if player is at boss boundary. Return boss info."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        boss_progress = self._get_boss_progress(tunnel)
        at_boss = self._at_boss_boundary(tunnel.get("depth", 0), boss_progress)

        if at_boss is None:
            return self._error("You're not at a boss boundary.")

        boss_name = BOSS_NAMES.get(at_boss, "Unknown Boss")
        attempts = tunnel.get("boss_attempts", 0) or 0

        # Get dialogue based on attempt count
        dialogue_list = BOSS_DIALOGUE.get(at_boss, ["..."])
        dialogue = dialogue_list[min(attempts, len(dialogue_list) - 1)]

        ascii_art = BOSS_ASCII.get(at_boss, "")

        return self._ok(
            boundary=at_boss,
            boss_name=boss_name,
            dialogue=dialogue,
            ascii_art=ascii_art,
            attempts=attempts,
            options=["cautious", "bold", "reckless"],
        )

    def fight_boss(self, discord_id: int, guild_id, risk_tier: str, wager: int = 0) -> dict:
        """
        Fight the boss at current boundary.

        risk_tier: 'cautious', 'bold', 'reckless'
        wager: JC to wager (0 for free fight)
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id
        boss_progress = self._get_boss_progress(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)

        if at_boss is None:
            return self._error("You're not at a boss boundary.")

        if risk_tier not in ("cautious", "bold", "reckless"):
            return self._error("Invalid risk tier. Choose: cautious, bold, reckless.")

        if wager < 0:
            return self._error("Wager must be non-negative.")

        if wager > 0:
            balance = self.player_repo.get_balance(discord_id, guild_id)
            if balance < wager:
                return self._error(f"You only have {balance} JC (wager: {wager}).")

        # Calculate odds using configured values
        odds_config = BOSS_ODDS.get(risk_tier, {})
        base_odds = odds_config.get("base", 0.50)

        # Get depth-specific payout multiplier from BOSS_PAYOUTS
        tier_index = {"cautious": 0, "bold": 1, "reckless": 2}.get(risk_tier, 1)
        payouts = BOSS_PAYOUTS.get(at_boss, (2.0, 3.0, 6.0))
        multiplier = payouts[tier_index] if tier_index < len(payouts) else 2.0

        # Depth scaling: harder bosses are deeper
        depth_penalty = (at_boss / 100) * 0.05
        prestige_level = tunnel.get("prestige_level", 0) or 0
        prestige_penalty = prestige_level * 0.02

        # Cheer bonus
        cheers = self._get_cheers(tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        win_chance = base_odds - depth_penalty - prestige_penalty + cheer_bonus
        win_chance = max(0.05, min(0.95, win_chance))

        # Free fights use separate (lower) odds from the config
        if wager == 0:
            free_odds = odds_config.get("free")
            if free_odds is not None:
                win_chance = free_odds - depth_penalty - prestige_penalty + cheer_bonus
                win_chance = max(0.05, min(0.95, win_chance))
            else:
                win_chance *= 0.85

        # Roll fight
        won = random.random() < win_chance
        boss_name = BOSS_NAMES.get(at_boss, "Unknown Boss")
        attempts = (tunnel.get("boss_attempts", 0) or 0) + 1

        if won:
            # Win: advance past boundary
            new_depth = at_boss
            jc_delta = int(wager * multiplier) if wager > 0 else random.randint(5, 15)

            boss_progress[str(at_boss)] = "defeated"
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                boss_progress=json.dumps(boss_progress),
                boss_attempts=0,
                cheer_data=None,  # Clear cheers
            )

            if wager > 0:
                self.player_repo.add_balance(discord_id, guild_id, int(wager * (multiplier - 1)))
            else:
                self.player_repo.add_balance(discord_id, guild_id, jc_delta)

            # Check Boss Slayer achievement
            achievements = self.check_achievements(
                discord_id, guild_id,
                {**tunnel, "depth": new_depth},
                {"action": "boss_win", "boundary": at_boss, "boss_progress": boss_progress},
            )

            dialogue_list = BOSS_DIALOGUE.get(at_boss, ["..."])
            defeat_msg = dialogue_list[-1] if dialogue_list else "Defeated!"

            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="boss_fight",
                details=json.dumps({
                    "boundary": at_boss, "won": True, "risk": risk_tier,
                    "wager": wager, "jc_delta": jc_delta,
                }),
            )

            return self._ok(
                won=True,
                boss_name=boss_name,
                boundary=at_boss,
                risk_tier=risk_tier,
                win_chance=round(win_chance, 2),
                jc_delta=jc_delta,
                payout=jc_delta,
                new_depth=new_depth,
                dialogue=defeat_msg,
                achievements=achievements,
            )
        else:
            # Lose: knocked back + lose wager
            knockback = random.randint(5, 10)
            new_depth = max(0, depth - knockback)
            jc_delta = -wager if wager > 0 else 0

            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                boss_attempts=attempts,
            )

            if wager > 0:
                self.player_repo.add_balance(discord_id, guild_id, -wager)

            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="boss_fight",
                details=json.dumps({
                    "boundary": at_boss, "won": False, "risk": risk_tier,
                    "wager": wager, "knockback": knockback,
                }),
            )

            return self._ok(
                won=False,
                boss_name=boss_name,
                boundary=at_boss,
                risk_tier=risk_tier,
                win_chance=round(win_chance, 2),
                jc_delta=jc_delta,
                knockback=knockback,
                new_depth=new_depth,
                dialogue=f"{boss_name} sends you flying back {knockback} blocks!",
                achievements=[],
            )

    def retreat_boss(self, discord_id: int, guild_id) -> dict:
        """Retreat from boss. Lose 1-3 blocks."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        boss_progress = self._get_boss_progress(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)

        if at_boss is None:
            return self._error("You're not at a boss boundary.")

        loss = random.randint(1, 3)
        new_depth = max(0, depth - loss)

        self.dig_repo.update_tunnel(discord_id, guild_id, depth=new_depth)
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="boss_retreat",
            details=json.dumps({"boundary": at_boss, "loss": loss}),
        )

        return self._ok(
            boundary=at_boss,
            loss=loss,
            new_depth=new_depth,
        )

    def scout_boss(self, discord_id: int, guild_id) -> dict:
        """Use a lantern to scout boss odds. Consumes lantern."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id
        boss_progress = self._get_boss_progress(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)

        if at_boss is None:
            return self._error("You're not at a boss boundary.")

        # Check for lantern
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        has_lantern = any(i.get("item_type") == "lantern" for i in inventory)
        if not has_lantern:
            return self._error("You need a Lantern to scout the boss.")

        # Consume lantern
        self.dig_repo.remove_inventory_item(discord_id, guild_id, "lantern")

        # Calculate odds for all tiers
        prestige_level = tunnel.get("prestige_level", 0) or 0
        depth_penalty = (at_boss / 100) * 0.05
        prestige_penalty = prestige_level * 0.02

        cheers = self._get_cheers(tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        payouts = BOSS_PAYOUTS.get(at_boss, (2.0, 3.0, 6.0))

        odds = {}
        for i, tier in enumerate(("cautious", "bold", "reckless")):
            cfg = BOSS_ODDS.get(tier, {})
            base = cfg.get("base", 0.50)
            chance = base - depth_penalty - prestige_penalty + cheer_bonus
            chance = max(0.05, min(0.95, chance))
            free_base = cfg.get("free", base * 0.85)
            free_chance = free_base - depth_penalty - prestige_penalty + cheer_bonus
            free_chance = max(0.05, min(0.95, free_chance))
            odds[tier] = {
                "win_pct": round(chance, 2),
                "free_fight_pct": round(free_chance, 2),
                "multiplier": payouts[i] if i < len(payouts) else 2.0,
            }

        return self._ok(
            boundary=at_boss,
            boss_name=BOSS_NAMES.get(at_boss, "Unknown Boss"),
            odds=odds,
        )

    def cheer_boss(self, cheerer_id: int, target_id: int, guild_id) -> dict:
        """Cheer for a player fighting a boss. Costs cooldown + 3 JC."""
        if cheerer_id == target_id:
            return self._error("You can't cheer for yourself.")

        target_tunnel = self.dig_repo.get_tunnel(target_id, guild_id)
        if target_tunnel is None:
            return self._error("That player doesn't have a tunnel.")

        target_tunnel = dict(target_tunnel)
        boss_progress = self._get_boss_progress(target_tunnel)
        at_boss = self._at_boss_boundary(target_tunnel.get("depth", 0), boss_progress)

        if at_boss is None:
            return self._error("That player is not at a boss boundary.")

        # Check cheerer cooldown
        cheerer_tunnel = self.dig_repo.get_tunnel(cheerer_id, guild_id)
        if cheerer_tunnel:
            cheerer_tunnel = dict(cheerer_tunnel)
            cheerer_tunnel["discord_id"] = cheerer_id
            cooldown = self._get_cooldown_remaining(cheerer_tunnel)
            if cooldown > 0:
                return self._error(f"You're on cooldown ({cooldown}s remaining).")

        # Check cost
        cost = 3
        balance = self.player_repo.get_balance(cheerer_id, guild_id)
        if balance < cost:
            return self._error(f"Cheering costs {cost} JC but you only have {balance} JC.")

        # Check max cheers (3 max = +15%)
        cheers = self._get_cheers(target_tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        if len(active_cheers) >= 3:
            return self._error("This player already has maximum cheers (3).")

        # Apply
        self.player_repo.add_balance(cheerer_id, guild_id, -cost)

        # Set cheerer cooldown
        if cheerer_tunnel:
            self.dig_repo.update_tunnel(cheerer_id, guild_id, last_dig_at=now)
        else:
            name = self.generate_tunnel_name()
            self.dig_repo.create_tunnel(cheerer_id, guild_id, name=name)
            self.dig_repo.update_tunnel(cheerer_id, guild_id, last_dig_at=now)

        # Add cheer
        active_cheers.append({
            "cheerer_id": cheerer_id,
            "expires_at": now + 3600,  # 1h
        })
        self.dig_repo.update_tunnel(
            target_id, guild_id,
            cheer_data=json.dumps(active_cheers),
        )

        boost = min(0.15, len(active_cheers) * 0.05)

        return self._ok(
            cost=cost,
            target_tunnel=target_tunnel.get("tunnel_name", "Unknown Tunnel"),
            total_boost=boost,
            cheer_count=len(active_cheers),
        )

    # ------------------------------------------------------------------
    # Prestige
    # ------------------------------------------------------------------

    def can_prestige(self, discord_id: int, guild_id) -> dict:
        """Check if player can prestige."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._ok(can_prestige=False, reason="No tunnel.")

        tunnel = dict(tunnel)
        boss_progress = self._get_boss_progress(tunnel)
        prestige_level = tunnel.get("prestige_level", 0) or 0

        all_defeated = all(v == "defeated" for v in boss_progress.values())
        at_max = prestige_level >= MAX_PRESTIGE

        can = all_defeated and not at_max
        reason = None
        if not all_defeated:
            remaining = [k for k, v in boss_progress.items() if v != "defeated"]
            reason = f"Bosses remaining: {', '.join(remaining)}"
        elif at_max:
            reason = f"Already at max prestige ({MAX_PRESTIGE})."

        return self._ok(
            can_prestige=can,
            reason=reason,
            prestige_level=prestige_level,
            available_perks=[p for p in PRESTIGE_PERKS if p not in self._get_prestige_perks(tunnel)],
        )

    def prestige(self, discord_id: int, guild_id, perk_choice: str) -> dict:
        """
        Prestige: reset tunnel, keep pickaxe, gain a perk.

        perk_choice: ID of the perk to select.
        """
        check = self.can_prestige(discord_id, guild_id)
        if not check.get("can_prestige"):
            return self._error(check.get("reason", "Cannot prestige."))

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        tunnel = dict(tunnel)

        # Validate perk choice
        valid_perks = list(PRESTIGE_PERKS)
        if perk_choice not in valid_perks:
            return self._error(f"Invalid perk. Choose from: {', '.join(valid_perks)}")

        current_perks = self._get_prestige_perks(tunnel)
        if perk_choice in current_perks:
            return self._error("You already have that perk.")

        current_perks.append(perk_choice)
        prestige_level = (tunnel.get("prestige_level", 0) or 0) + 1

        # Reset tunnel
        boss_progress = {str(b): "active" for b in BOSS_BOUNDARIES}
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=0,
            boss_progress=json.dumps(boss_progress),
            boss_attempts=0,
            prestige_level=prestige_level,
            prestige_perks=json.dumps(current_perks),
            cheer_data=None,
            injury_state=None,
        )

        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="prestige",
            details=json.dumps({
                "level": prestige_level, "perk": perk_choice,
            }),
        )

        return self._ok(
            prestige_level=prestige_level,
            perk_chosen=perk_choice,
            perks=current_perks,
        )

    # ------------------------------------------------------------------
    # Items
    # ------------------------------------------------------------------

    def use_item(self, discord_id: int, guild_id, item_type: str) -> dict:
        """Queue an item for next dig."""
        if item_type not in CONSUMABLE_ITEMS:
            return self._error(f"Unknown item type: {item_type}")

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)

        # Check inventory
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        has_item = any(i.get("item_type") == item_type for i in inventory)
        if not has_item:
            return self._error(f"You don't have a {CONSUMABLE_ITEMS[item_type]['name']}.")

        # Check not already queued
        queued = self._get_queued_items_for_tunnel(discord_id, guild_id)
        if any(q.get("type") == item_type for q in queued):
            return self._error(f"{CONSUMABLE_ITEMS[item_type]['name']} is already queued.")

        # Find the first non-queued item of this type and queue it
        for inv_item in inventory:
            if inv_item.get("item_type") == item_type and not inv_item.get("queued"):
                self.dig_repo.queue_item(inv_item["id"])
                break

        return self._ok(
            item=CONSUMABLE_ITEMS[item_type]["name"],
            queued=True,
        )

    def queue_item(self, discord_id: int, guild_id, item_id: int) -> dict:
        """Queue a specific inventory item by its database id."""
        self.dig_repo.queue_item(item_id)
        return self._ok(queued=True)

    def buy_item(self, discord_id: int, guild_id, item_type: str) -> dict:
        """Buy an item from the shop."""
        if item_type not in ITEM_PRICES:
            return self._error(f"Unknown item type: {item_type}")

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel. Dig first!")

        # Check inventory capacity
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        if len(inventory) >= MAX_INVENTORY_SIZE:
            return self._error(f"Inventory full ({MAX_INVENTORY_SIZE} items max).")

        price = ITEM_PRICES[item_type]
        balance = self.player_repo.get_balance(discord_id, guild_id)
        if balance < price:
            return self._error(f"Costs {price} JC but you only have {balance} JC.")

        self.player_repo.add_balance(discord_id, guild_id, -price)
        item_id = self.dig_repo.add_inventory_item(discord_id, guild_id, item_type)

        item_name = CONSUMABLE_ITEMS.get(item_type, {}).get("name", item_type)

        return self._ok(
            item=item_name,
            item_id=item_id,
            cost=price,
            balance_after=balance - price,
        )

    def get_inventory(self, discord_id: int, guild_id) -> list[dict]:
        """Return inventory items with names and queued status."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return []

        tunnel = dict(tunnel)
        items = self.dig_repo.get_inventory(discord_id, guild_id)
        queued = self._get_queued_items_for_tunnel(discord_id, guild_id)
        queued_types = {q.get("type") for q in queued}

        result = []
        for item in items:
            itype = item.get("item_type", "unknown")
            info = CONSUMABLE_ITEMS.get(itype, {})
            result.append({
                "type": itype,
                "name": info.get("name", itype),
                "description": info.get("description", ""),
                "queued": itype in queued_types,
            })

        return result

    # ------------------------------------------------------------------
    # Defense
    # ------------------------------------------------------------------

    def set_trap(self, discord_id: int, guild_id) -> dict:
        """Set a trap on your tunnel."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)

        if tunnel.get("trap_active"):
            return self._error("You already have an active trap.")

        today = self._get_game_date()
        trap_date = tunnel.get("trap_date")
        trap_free_today = tunnel.get("trap_free_today", 0) or 0

        cost = 0
        if trap_date != today:
            # Reset free trap for new day
            trap_free_today = 0

        if trap_free_today > 0:
            # Already used free trap today — pay
            cost = 5 + (tunnel.get("depth", 0) // 25)
            balance = self.player_repo.get_balance(discord_id, guild_id)
            if balance < cost:
                return self._error(f"Trap costs {cost} JC but you only have {balance} JC.")
            self.player_repo.add_balance(discord_id, guild_id, -cost)

        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            trap_active=1,
            trap_free_today=trap_free_today + 1,
            trap_date=today,
        )

        return self._ok(cost=cost, message="Trap set!")

    def buy_insurance(self, discord_id: int, guild_id) -> dict:
        """Buy 24h sabotage insurance."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        depth = tunnel["depth"] if tunnel else 0
        cost = 5 + depth // 25
        balance = self.player_repo.get_balance(discord_id, guild_id)
        if balance < cost:
            return self._error(f"Insurance costs {cost} JC but you only have {balance} JC.")

        now = int(time.time())
        self.player_repo.add_balance(discord_id, guild_id, -cost)
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            insured_until=now + 86400,  # 24h
        )

        return self._ok(cost=cost, expires_at=now + 86400)

    # ------------------------------------------------------------------
    # Artifacts
    # ------------------------------------------------------------------

    def roll_artifact(self, discord_id: int, guild_id, depth: int) -> dict | None:
        """
        Roll for an artifact drop. Returns artifact info or None.

        Rates: common 5%, uncommon 2%, rare 0.5%, legendary 0.1%
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return None

        tunnel = dict(tunnel)
        layer = self._get_layer(depth)
        layer_name = layer.get("name", "dirt")

        # Echo Stone relic bonus
        rate_mod = 1.1 if self._has_relic(discord_id, guild_id, "echo_stone") else 1.0

        # Roll for each rarity tier
        tiers = [
            ("common", 0.05),
            ("uncommon", 0.02),
            ("rare", 0.005),
            ("legendary", 0.001),
        ]

        hit_rarity = None
        for rarity, base_rate in reversed(tiers):  # Check legendary first
            if random.random() < base_rate * rate_mod:
                hit_rarity = rarity
                break

        if hit_rarity is None:
            return None

        # Pick from pool
        eligible = [
            a for a in ARTIFACT_POOL
            if a.get("rarity") == hit_rarity
            and layer_name in a.get("layers", [layer_name])
        ]

        if not eligible:
            # Fallback: any artifact of that rarity
            eligible = [a for a in ARTIFACT_POOL if a.get("rarity") == hit_rarity]

        if not eligible:
            return None

        artifact = random.choice(eligible)

        # Add to player artifacts
        self.dig_repo.add_artifact(
            discord_id, guild_id,
            artifact_id=artifact["id"],
            name=artifact["name"],
            rarity=hit_rarity,
            artifact_type=artifact.get("type", "trophy"),
        )

        # Register in guild museum
        self.dig_repo.register_museum_artifact(
            guild_id,
            artifact_id=artifact["id"],
            first_finder_id=discord_id,
        )

        return {
            "id": artifact["id"],
            "name": artifact["name"],
            "rarity": hit_rarity,
            "type": artifact.get("type", "trophy"),
            "description": artifact.get("description", ""),
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

        # Transfer
        self.dig_repo.remove_artifact(target_artifact["id"])
        self.dig_repo.add_artifact(
            receiver_id, guild_id,
            target_artifact["artifact_id"],
            is_relic=True,
        )

        # If was equipped on giver, unequip
        relics = self._get_equipped_relics_for_player(giver_id, guild_id)
        for r in relics:
            if r.get("artifact_id") == target_artifact.get("artifact_id"):
                self.dig_repo.unequip_relic(r["id"])

        return self._ok(
            artifact_id=artifact_id,
            artifact_name=target_artifact.get("name", "Unknown"),
        )

    def get_collection(self, discord_id: int, guild_id) -> dict:
        """Return all artifacts grouped by layer and rarity."""
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        collection = {}
        for a in artifacts:
            a = dict(a)
            rarity = a.get("rarity", "common")
            if rarity not in collection:
                collection[rarity] = []
            collection[rarity].append(a)
        return {"artifacts": collection, "total": len(artifacts)}

    # ------------------------------------------------------------------
    # Museum
    # ------------------------------------------------------------------

    def get_museum(self, guild_id) -> dict:
        """Return guild artifact registry with first finders and counts."""
        entries = self.dig_repo.get_registry(guild_id)
        entries = [dict(e) for e in entries]

        # Group by layer
        by_layer = {}
        for e in entries:
            # Look up artifact info from pool
            art_info = next(
                (a for a in ARTIFACT_POOL if a["id"] == e.get("artifact_id")),
                None,
            )
            layer = "unknown"
            if art_info:
                layers = art_info.get("layers", [])
                layer = layers[0] if layers else "unknown"

            if layer not in by_layer:
                by_layer[layer] = []
            by_layer[layer].append(e)

        return {
            "entries": entries,
            "by_layer": by_layer,
            "total_discovered": len(entries),
            "total_possible": len(ARTIFACT_POOL),
        }

    # ------------------------------------------------------------------
    # Events
    # ------------------------------------------------------------------

    def roll_event(self, depth: int, luminosity: int = 100) -> dict | None:
        """
        Roll for a random event with layer-specific rates and rarity.

        Returns event info dict, or None if no event triggers.
        """
        layer = self._get_layer(depth)
        layer_name = layer.get("name", "Dirt")
        is_pitch_black = luminosity <= 0

        # Filter eligible events by depth, layer, and darkness requirement
        eligible = [
            e for e in EVENT_POOL
            if depth >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth <= e["max_depth"])
            and (e.get("layer") is None or e["layer"] == layer_name)
            and (not e.get("requires_dark") or is_pitch_black)
        ]

        # Non-darkness events are excluded at pitch black if darkness events exist
        if is_pitch_black:
            dark_events = [e for e in eligible if e.get("requires_dark")]
            if dark_events:
                eligible = dark_events + [e for e in eligible if not e.get("requires_dark")]

        if not eligible:
            return None

        # Rarity-weighted selection
        weights = {"common": 70, "uncommon": 20, "rare": 8, "legendary": 2}
        weighted = [(e, weights.get(e.get("rarity", "common"), 70)) for e in eligible]
        events, w = zip(*weighted)
        event = random.choices(events, weights=w, k=1)[0]

        return {
            "id": event["id"],
            "name": event["name"],
            "description": event["description"],
            "complexity": event.get("complexity", "choice"),
            "safe_option": event.get("safe_option"),
            "risky_option": event.get("risky_option"),
            "buff_on_success": event.get("buff_on_success"),
            "rarity": event.get("rarity", "common"),
        }

    def resolve_event(self, discord_id: int, guild_id, event_id: str, choice: str) -> dict:
        """Apply event outcome based on safe/risky choice."""
        event = next((e for e in EVENT_POOL if e["id"] == event_id), None)
        if event is None:
            return self._error("Unknown event.")

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        depth = tunnel.get("depth", 0)

        outcomes = event.get("outcomes", {})
        outcome = outcomes.get(choice)
        if outcome is None:
            return self._error(f"Invalid choice: {choice}")

        # Apply outcome
        jc_delta = 0
        depth_delta = 0
        message = outcome.get("message", "Nothing happened.")

        if "jc" in outcome:
            jc_range = outcome["jc"]
            if isinstance(jc_range, list):
                jc_delta = random.randint(jc_range[0], jc_range[1])
            else:
                jc_delta = jc_range
            self.player_repo.add_balance(discord_id, guild_id, jc_delta)

        if "depth" in outcome:
            depth_range = outcome["depth"]
            if isinstance(depth_range, list):
                depth_delta = random.randint(depth_range[0], depth_range[1])
            else:
                depth_delta = depth_range
            new_depth = max(0, depth + depth_delta)
            self.dig_repo.update_tunnel(discord_id, guild_id, depth=new_depth)

        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="event",
            details=json.dumps({
                "event_id": event_id, "choice": choice,
                "jc_delta": jc_delta, "depth_delta": depth_delta,
            }),
        )

        return self._ok(
            event_name=event.get("name", "Unknown Event"),
            choice=choice,
            jc_delta=jc_delta,
            depth_delta=depth_delta,
            message=message,
        )

    # ------------------------------------------------------------------
    # Achievements
    # ------------------------------------------------------------------

    def check_achievements(self, discord_id: int, guild_id, tunnel: dict, context: dict) -> list[dict]:
        """
        Check all achievement conditions. Return newly unlocked achievements.

        context: dict with what just happened (action, advance, boss_win, etc.)
        """
        existing = self.dig_repo.get_achievements(discord_id, guild_id)
        existing_ids = {a.get("achievement_id") for a in existing}

        newly_unlocked = []

        for ach in ACHIEVEMENTS:
            if ach["id"] in existing_ids:
                continue

            unlocked = False
            condition = ach.get("condition", {})
            ctype = condition.get("type")

            if ctype == "depth":
                if tunnel.get("depth", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "total_digs":
                if tunnel.get("total_digs", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "streak":
                if tunnel.get("streak_days", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "boss_win":
                if context.get("action") == "boss_win":
                    unlocked = True
            elif ctype == "all_bosses":
                bp = context.get("boss_progress") or self._get_boss_progress(tunnel)
                if all(v == "defeated" for v in bp.values()):
                    unlocked = True
            elif ctype == "prestige":
                if tunnel.get("prestige_level", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "cave_in" and context.get("action") == "cave_in":
                unlocked = True

            if unlocked:
                self.dig_repo.add_achievement(
                    discord_id, guild_id,
                    achievement_id=ach["id"],
                    name=ach["name"],
                )
                newly_unlocked.append({
                    "id": ach["id"],
                    "name": ach["name"],
                    "description": ach.get("description", ""),
                    "reward": ach.get("reward", 0),
                })

                # Award JC reward
                if ach.get("reward", 0) > 0:
                    self.player_repo.add_balance(discord_id, guild_id, ach["reward"])

        return newly_unlocked

    # ------------------------------------------------------------------
    # Abandon Tunnel
    # ------------------------------------------------------------------

    def abandon_tunnel(self, discord_id: int, guild_id) -> dict:
        """Abandon tunnel for a small JC refund."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        depth = tunnel.get("depth", 0)

        if depth < 10:
            return self._error("Tunnel must be at least 10 blocks deep to abandon.")

        # Check 24h cooldown
        recent_abandons = self.dig_repo.get_recent_actions(
            discord_id, guild_id, action_type="abandon", hours=24
        )
        if recent_abandons:
            return self._error("You can only abandon once every 24 hours.")

        refund = int(depth * 0.1)

        # Reset tunnel (keep prestige, pickaxe, name)
        boss_progress = {str(b): "active" for b in BOSS_BOUNDARIES}
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=0,
            boss_progress=json.dumps(boss_progress),
            boss_attempts=0,
            injury_state=None,
            cheer_data=None,
            streak_days=0,
        )

        self.player_repo.add_balance(discord_id, guild_id, refund)

        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="abandon",
            details=json.dumps({"depth": depth, "refund": refund}),
        )

        return self._ok(
            depth_lost=depth,
            refund=refund,
        )

    # ------------------------------------------------------------------
    # Stats & Utility
    # ------------------------------------------------------------------

    def get_flex_data(self, discord_id: int, guild_id) -> dict:
        """Return tunnel info, achievements, titles, prestige emoji, stats."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("No tunnel found.")

        tunnel = dict(tunnel)
        achievements = self.dig_repo.get_achievements(discord_id, guild_id)

        boss_progress = self._get_boss_progress(tunnel)
        all_bosses_beaten = all(v == "defeated" for v in boss_progress.values())

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
            achievement_count=len(achievements),
            streak=tunnel.get("streak_days", 0) or 0,
            layer=self._get_layer(tunnel.get("depth", 0)).get("name", "dirt"),
        )

    def get_guild_stats(self, guild_id) -> dict:
        """Aggregate stats for the guild."""
        tunnels = self.dig_repo.get_all_tunnels(guild_id)
        tunnels = [dict(t) for t in tunnels]

        if not tunnels:
            return self._ok(
                total_digs=0,
                total_depth=0,
                total_jc_earned=0,
                most_active=None,
                deepest=None,
                tunnel_count=0,
            )

        total_digs = sum(t.get("total_digs", 0) or 0 for t in tunnels)
        total_depth = sum(t.get("depth", 0) or 0 for t in tunnels)
        total_jc = sum(t.get("total_jc_earned", 0) or 0 for t in tunnels)

        most_active = max(tunnels, key=lambda t: t.get("total_digs", 0) or 0)
        deepest = max(tunnels, key=lambda t: t.get("depth", 0) or 0)

        return self._ok(
            total_digs=total_digs,
            total_depth=total_depth,
            total_jc_earned=total_jc,
            most_active={
                "discord_id": most_active.get("discord_id"),
                "name": most_active.get("tunnel_name"),
                "total_digs": most_active.get("total_digs", 0),
            },
            deepest={
                "discord_id": deepest.get("discord_id"),
                "name": deepest.get("tunnel_name"),
                "depth": deepest.get("depth", 0),
            },
            tunnel_count=len(tunnels),
        )
