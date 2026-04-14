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
    ASCENSION_MODIFIERS,
    BOSS_ASCII,
    BOSS_BOUNDARIES,
    BOSS_DIALOGUE,
    BOSS_NAMES,
    BOSS_ODDS,
    BOSS_PAYOUTS,
    BOSS_PHASE2,
    CONSUMABLE_ITEMS,
    CORRUPTION_BAD,
    CORRUPTION_WEIRD,
    DIG_TIPS,
    EVENT_CHAIN_CHANCE,
    EVENT_POOL,
    FREE_DIG_COOLDOWN,
    INJURY_SLOW_COOLDOWN,
    ITEM_PRICES,
    LAYER_WEATHER_POOL,
    LAYERS,
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK,
    LUMINOSITY_DARK_CAVE_IN_BONUS,
    LUMINOSITY_DARK_EVENT_MULTIPLIER,
    LUMINOSITY_DARK_JC_MULTIPLIER,
    LUMINOSITY_DARK_RISKY_PENALTY,
    LUMINOSITY_DIM,
    LUMINOSITY_DIM_CAVE_IN_BONUS,
    LUMINOSITY_DIM_EVENT_MULTIPLIER,
    LUMINOSITY_DRAIN_PER_DIG,
    LUMINOSITY_MAX,
    LUMINOSITY_PITCH_BLACK,
    LUMINOSITY_PITCH_CAVE_IN_BONUS,
    LUMINOSITY_PITCH_EVENT_MULTIPLIER,
    LUMINOSITY_PITCH_FORCE_RISKY,
    LUMINOSITY_PITCH_JC_MULTIPLIER,
    MAX_INVENTORY_SIZE,
    MAX_PRESTIGE,
    MILESTONES,
    MUTATION_BY_ID,
    MUTATIONS_POOL,
    PAID_DIG_COSTS,
    PICKAXE_TIERS,
    PRESTIGE_PERKS,
    STREAKS,
    TUNNEL_NAME_ADJECTIVES,
    TUNNEL_NAME_NOUNS,
    TUNNEL_NAME_SILLY,
    TUNNEL_NAME_TITLES,
    WEATHER_BY_ID,
)

logger = logging.getLogger("cama_bot.services.dig")

RARITY_WEIGHTS = {"common": 70, "uncommon": 20, "rare": 12, "legendary": 4}
DIG_STARTING_STAT_POINTS = 5
DIG_BOSS_STAT_POINT_BONUS = 1
MINER_BACKSTORY_MAX_LENGTH = 600
STRENGTH_MAX_ADVANCE_INTERVAL = 2
STRENGTH_MIN_ADVANCE_INTERVAL = 5
SMARTS_CAVE_IN_REDUCTION = 0.02
STAMINA_COOLDOWN_REDUCTION = 0.04
STAMINA_MAX_REDUCTION = 0.50

# Pre-compute which event IDs have art assets (disk or PIL).
# Lazily initialized on first use to avoid import-time side effects.
_EVENTS_WITH_ART: set[str] | None = None


def _get_events_with_art() -> set[str]:
    """Return the set of event IDs that have art (on-disk or PIL-generated)."""
    global _EVENTS_WITH_ART  # noqa: PLW0603
    if _EVENTS_WITH_ART is not None:
        return _EVENTS_WITH_ART
    try:
        import os

        from utils.dig_drawing import has_event_scene

        art_dir = os.path.join(os.path.dirname(os.path.dirname(__file__)), "assets", "dig", "events")
        disk = set()
        if os.path.isdir(art_dir):
            disk = {f.split(".")[0] for f in os.listdir(art_dir)}
        pil = {eid for e in EVENT_POOL if has_event_scene(eid := e["id"])}
        _EVENTS_WITH_ART = disk | pil
    except Exception:
        _EVENTS_WITH_ART = set()
    return _EVENTS_WITH_ART


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
        cooldown = FREE_DIG_COOLDOWN
        # Mutation: restless — extra cooldown
        mutations = self._get_mutations(tunnel)
        mutation_fx = self._apply_mutation_effects(mutations)
        cooldown += int(mutation_fx.get("cooldown_bonus_seconds", 0))
        # Check for stun from injury
        injury = json.loads(tunnel["injury_state"]) if tunnel.get("injury_state") else None
        if injury and injury.get("type") == "slower_cooldown":
            cooldown = INJURY_SLOW_COOLDOWN
        cooldown = self._apply_stamina_to_cooldown(cooldown, tunnel)
        remaining = cooldown - elapsed
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

    def _is_unstarted_tunnel(self, tunnel: dict) -> bool:
        """True for profile-created tunnels that have not had a first dig yet."""
        return (
            (tunnel.get("total_digs", 0) or 0) == 0
            and tunnel.get("last_dig_at") is None
            and (tunnel.get("depth", 0) or 0) == 0
        )

    # ── Layer Weather ────────────────────────────────────────────────

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

    def _get_prestige_perks(self, tunnel: dict) -> list[str]:
        """Get list of active prestige perks."""
        raw = tunnel.get("prestige_perks")
        if not raw:
            return []
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return []

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
        """Return the boss boundary if depth is exactly at one and boss is active or in phase 1."""
        for b in BOSS_BOUNDARIES:
            status = boss_progress.get(str(b))
            if depth == b - 1 and status in ("active", "phase1_defeated"):
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
        # Frostforged / Void-Touched pickaxe: -25% luminosity drain
        pickaxe_tier = tunnel.get("pickaxe_tier", 0) or 0
        if pickaxe_tier >= 5:  # Frostforged or better
            drain = max(0, drain - drain // 4)
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

    # ------------------------------------------------------------------
    # Ascension System Helpers
    # ------------------------------------------------------------------

    def _get_ascension_effects(self, prestige_level: int) -> dict:
        """Return cumulative ascension effects for all active levels."""
        effects: dict = {}
        for lvl in range(1, prestige_level + 1):
            mod = ASCENSION_MODIFIERS.get(lvl)
            if mod is None:
                continue
            for key, value in mod.effects.items():
                if isinstance(value, bool):
                    effects[key] = value
                elif isinstance(value, (int, float)):
                    effects[key] = effects.get(key, 0) + value
        return effects

    def _roll_corruption(self, prestige_level: int) -> dict | None:
        """Roll a corruption effect for P6+. Returns effect dict or None."""
        if prestige_level < 6:
            return None
        if random.random() < 0.80:
            effect = random.choice(CORRUPTION_BAD)
        else:
            effect = random.choice(CORRUPTION_WEIRD)
        return {"id": effect.id, "description": effect.description,
                "weird": effect.weird, "effects": dict(effect.effects)}

    def _get_mutations(self, tunnel: dict) -> list[dict]:
        """Get active mutations from tunnel JSON."""
        raw = tunnel.get("mutations")
        if not raw:
            return []
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return []

    def _apply_mutation_effects(self, mutations: list[dict]) -> dict:
        """Return combined mutation effects dict."""
        combined: dict = {}
        for m in mutations:
            mut_def = MUTATION_BY_ID.get(m.get("id", ""))
            if mut_def is None:
                continue
            for key, value in mut_def.effects.items():
                if isinstance(value, bool):
                    combined[key] = value
                elif isinstance(value, (int, float)):
                    combined[key] = combined.get(key, 0) + value
        return combined

    def _roll_mutations_for_prestige(self) -> tuple[dict, list[dict]]:
        """Roll mutations for P8+: 1 forced random + 3 choices to pick 1 from."""
        pool = list(MUTATIONS_POOL)
        random.shuffle(pool)
        forced = pool[0]
        remaining = [m for m in pool[1:] if m.id != forced.id]
        choices = remaining[:3]
        forced_dict = {"id": forced.id, "name": forced.name,
                       "description": forced.description, "positive": forced.positive}
        choices_dicts = [{"id": m.id, "name": m.name,
                          "description": m.description, "positive": m.positive}
                         for m in choices]
        return forced_dict, choices_dicts

    def _chain_event(self, depth: int, prestige_level: int,
                     trigger_rarity: str, luminosity: int = 100) -> dict | None:
        """P7+: 25% chance to chain another event of same or higher rarity."""
        if prestige_level < 7:
            return None
        if random.random() >= EVENT_CHAIN_CHANCE:
            return None
        rarity_order = ["common", "uncommon", "rare", "legendary"]
        min_idx = rarity_order.index(trigger_rarity) if trigger_rarity in rarity_order else 0
        allowed_rarities = set(rarity_order[min_idx:])
        eligible = [
            e for e in EVENT_POOL
            if depth >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth <= e["max_depth"])
            and e.get("rarity", "common") in allowed_rarities
            and prestige_level >= e.get("min_prestige", 0)
        ]
        if not eligible:
            return None
        weighted = [(e, RARITY_WEIGHTS.get(e.get("rarity", "common"), 70)) for e in eligible]
        events, w = zip(*weighted)
        event = random.choices(events, weights=w, k=1)[0]
        return {
            "id": event["id"],
            "name": event["name"],
            "description": event["description"],
            "complexity": event.get("complexity", "choice"),
            "safe_option": event.get("safe_option"),
            "risky_option": event.get("risky_option"),
            "desperate_option": event.get("desperate_option"),
            "boon_options": event.get("boon_options"),
            "buff_on_success": event.get("buff_on_success"),
            "rarity": event.get("rarity", "common"),
            "chained": True,
        }

    def _calculate_run_score(self, tunnel: dict) -> int:
        """Calculate prestige run score."""
        depth = tunnel.get("depth", 0) or 0
        boss_progress = self._get_boss_progress(tunnel)
        bosses_defeated = sum(1 for v in boss_progress.values() if v == "defeated")
        run_jc = tunnel.get("current_run_jc", 0) or 0
        run_artifacts = tunnel.get("current_run_artifacts", 0) or 0
        run_events = tunnel.get("current_run_events", 0) or 0
        prestige_level = tunnel.get("prestige_level", 0) or 0

        base = (depth * 1 + bosses_defeated * 50
                + int(run_jc * 0.5) + run_artifacts * 25 + run_events * 10)
        multiplier = 1 + prestige_level * 0.1
        # P10 "The Endless" doubles score multiplier
        ascension = self._get_ascension_effects(prestige_level)
        score_mult = ascension.get("score_multiplier", 0)
        if score_mult:
            multiplier += score_mult
        return int(base * multiplier)

    def get_hall_of_fame(self, guild_id) -> dict:
        """Get guild leaderboard of best prestige run scores."""
        rows = self.dig_repo.get_hall_of_fame(guild_id)
        entries = []
        for row in rows:
            r = dict(row) if not isinstance(row, dict) else row
            entries.append({
                "discord_id": r.get("discord_id"),
                "tunnel_name": r.get("tunnel_name", "Unknown"),
                "prestige_level": r.get("prestige_level", 0),
                "best_run_score": r.get("best_run_score", 0),
            })
        return self._ok(entries=entries)

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

        # P5 ascension: decay rate multiplier
        prestige_lvl = tunnel.get("prestige_level", 0) or 0
        asc_decay = self._get_ascension_effects(prestige_lvl).get("decay_multiplier", 0)
        ascension_decay_mult = 1.0 + asc_decay if asc_decay else 1.0

        total_decay = int(base_rate * days_elapsed * helper_multiplier * relic_multiplier * ascension_decay_mult)

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

    def _build_parked_boss_return(
        self, tunnel: dict, discord_id: int, guild_id, decay_info
    ) -> dict | None:
        """If the tunnel is already at a defeated-eligible boss boundary, return
        a boss-encounter result dict so /dig stops here without charging cooldown
        or paid fees. Returns ``None`` if the tunnel is not parked at a boundary."""
        depth_before = tunnel.get("depth", 0)
        boss_progress_early = self._get_boss_progress(tunnel)
        at_boss_early = self._at_boss_boundary(depth_before, boss_progress_early)
        if at_boss_early is None:
            return None

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
            items_used_ids=[],
            pickaxe_tier=tunnel.get("pickaxe_tier", 0) or 0,
            tip="A boss blocks your path!",
            decay_info=decay_info,
            luminosity_info=None,
        )

    def _execute_first_dig(
        self, discord_id: int, guild_id, tunnel: dict, depth_before: int, now: int, today: str, decay_info
    ) -> dict:
        """Run the first-ever dig for a tunnel: guaranteed safe, writes the
        initial depth/streak/run counters, awards small JC, returns a welcome
        result dict."""
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
            items_used_ids=[],
            pickaxe_tier=0,
            tip="Welcome to the mines! Use /dig again after the cooldown.",
            decay_info=decay_info,
        )

    def _resolve_queued_items(
        self, discord_id: int, guild_id
    ) -> tuple[list[str], list[str], dict[str, bool]]:
        """Pop queued items from the inventory and return display names, ids, and
        a flag-map (one ``has_<item>`` key per consumable) for the main dig loop."""
        queued = self._get_queued_items_for_tunnel(discord_id, guild_id)
        items_used: list[str] = []
        items_used_ids: list[str] = []
        flags = {
            "has_dynamite": False,
            "has_hard_hat": False,
            "has_lantern": False,
            "has_torch": False,
            "has_grappling_hook": False,
            "has_depth_charge": False,
            "has_reinforcement": False,
            "has_sonar_pulse": False,
            "has_void_bait": False,
        }
        _display_names = {
            "dynamite": "Dynamite",
            "hard_hat": "Hard Hat",
            "lantern": "Lantern",
            "torch": "Torch",
            "grappling_hook": "Grappling Hook",
            "depth_charge": "Depth Charge",
            "reinforcement": "Reinforcement",
            "sonar_pulse": "Sonar Pulse",
            "void_bait": "Void Bait",
        }
        for item in queued:
            itype = item.get("type")
            if itype in _display_names:
                items_used.append(_display_names[itype])
            flag_key = f"has_{itype}" if itype else None
            if flag_key and flag_key in flags:
                flags[flag_key] = True
            if itype:
                items_used_ids.append(itype)

        if queued:
            for item in queued:
                self.dig_repo.remove_inventory_item(discord_id, guild_id, item.get("type"))
            self.dig_repo.unqueue_all(discord_id, guild_id)

        return items_used, items_used_ids, flags

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
        elif self._is_unstarted_tunnel(dict(tunnel)):
            is_first_dig = True

        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id

        # 2. Apply lazy decay
        decay_info = self._apply_lazy_decay(tunnel, guild_id)

        depth_before = tunnel.get("depth", 0)

        # 2b. If already at a boss boundary, return boss encounter without
        #     consuming cooldown or charging for paid dig.
        if not is_first_dig:
            parked_return = self._build_parked_boss_return(
                tunnel, discord_id, guild_id, decay_info
            )
            if parked_return is not None:
                return parked_return

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
                    preview_cost = self._calculate_paid_dig_cost(tunnel, pc)
                    return {
                        "success": False,
                        "error": f"Dig on cooldown ({cooldown_remaining}s remaining).",
                        "cooldown_remaining": cooldown_remaining,
                        "paid_dig_cost": preview_cost,
                        "paid_dig_available": True,
                    }

                # Paid dig requested
                paid_date = tunnel.get("paid_dig_date")
                paid_count = tunnel.get("paid_digs_today") or 0

                if paid_date != today:
                    paid_count = 0

                paid_dig_cost = self._calculate_paid_dig_cost(tunnel, paid_count)

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
            return self._execute_first_dig(
                discord_id, guild_id, tunnel, depth_before, now, today, decay_info
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
        items_used, items_used_ids, _item_flags = self._resolve_queued_items(discord_id, guild_id)
        has_dynamite = _item_flags["has_dynamite"]
        has_hard_hat = _item_flags["has_hard_hat"]
        has_lantern = _item_flags["has_lantern"]
        has_torch = _item_flags["has_torch"]
        has_grappling_hook = _item_flags["has_grappling_hook"]
        has_depth_charge = _item_flags["has_depth_charge"]
        has_reinforcement = _item_flags["has_reinforcement"]
        has_sonar_pulse = _item_flags["has_sonar_pulse"]
        has_void_bait = _item_flags["has_void_bait"]

        # Hard hat: grant 3 charges of full cave-in prevention
        if has_hard_hat:
            existing_charges = tunnel.get("hard_hat_charges", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                hard_hat_charges=existing_charges + 3,
            )
            tunnel["hard_hat_charges"] = existing_charges + 3

        # Reinforcement: prevent decay for 48h and reduce sabotage damage
        if has_reinforcement:
            reinforced_until_ts = now + 48 * 3600
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                reinforced_until=reinforced_until_ts,
            )

        # Void Bait: double event chance for next 3 digs
        if has_void_bait:
            existing_vb = tunnel.get("void_bait_digs", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                void_bait_digs=existing_vb + 3,
            )
            tunnel["void_bait_digs"] = existing_vb + 3

        # 7. Get layer info
        layer = self._get_layer(depth_before)

        # 7b. Apply luminosity drain
        layer_name = layer.get("name", "Dirt")

        # 7a. Get layer weather effects
        weather_fx = self._get_weather_effects(guild_id, layer_name)
        weather_info = None
        if weather_fx:
            # Find the weather entry for display
            for entry in self._ensure_weather(guild_id):
                if entry.get("layer_name") == layer_name:
                    w = WEATHER_BY_ID.get(entry.get("weather_id"))
                    if w:
                        weather_info = {"name": w.name, "description": w.description}

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

        # 8. Prestige perks, relics, and ASCENSION
        perks = self._get_prestige_perks(tunnel)
        prestige_level = tunnel.get("prestige_level", 0) or 0
        ascension = self._get_ascension_effects(prestige_level)

        # 8a. Roll corruption (P6+)
        corruption = self._roll_corruption(prestige_level)

        # 8b. Get mutation effects (P8+)
        mutations = self._get_mutations(tunnel)
        mutation_fx = self._apply_mutation_effects(mutations)

        # 8c. Apply ascension luminosity drain bonus (P3+)
        extra_drain = ascension.get("luminosity_drain_multiplier", 0)
        if extra_drain > 0 and lum_info["drained"] > 0:
            bonus_drain = int(lum_info["drained"] * extra_drain)
            luminosity = max(0, luminosity - bonus_drain)
            lum_info["luminosity_after"] = luminosity
            lum_info["drained"] += bonus_drain
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

        # 8d. Apply weather luminosity drain modifier
        weather_drain = weather_fx.get("luminosity_drain_multiplier", 0)
        if weather_drain > 0 and lum_info["drained"] > 0:
            bonus_drain = int(lum_info["drained"] * weather_drain)
            luminosity = max(0, luminosity - bonus_drain)
            lum_info["luminosity_after"] = luminosity
            lum_info["drained"] += bonus_drain
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

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
        miner_stats = self._get_miner_stats(tunnel)
        stat_effects = self._get_stat_effects(miner_stats)

        # 9. Cave-in check (with ascension + corruption + mutation modifiers)
        hard_hat_charges = tunnel.get("hard_hat_charges", 0) or 0
        cave_in_chance = layer.get("cave_in_pct", 0.10)
        # Ascension cave-in bonus
        cave_in_chance += ascension.get("cave_in_bonus", 0)
        # Weather cave-in modifier
        cave_in_chance += weather_fx.get("cave_in_bonus", 0)
        # Corruption cave-in bonus (one-dig)
        if corruption:
            cave_in_chance += corruption["effects"].get("cave_in_bonus", 0)
        # dark_adaptation perk: dim luminosity has no cave-in penalty
        lum_cave_bonus = self._luminosity_cave_in_bonus(luminosity)
        if "dark_adaptation" in perks and luminosity >= LUMINOSITY_DIM and luminosity < LUMINOSITY_BRIGHT:
            lum_cave_bonus = 0.0
        # Mutation: dark_sight ignores luminosity cave-in penalty
        if mutation_fx.get("ignore_luminosity_cave_in"):
            lum_cave_bonus = 0.0
        cave_in_chance += lum_cave_bonus
        cave_in_chance -= perk_cavein_reduction
        cave_in_chance -= pickaxe_cavein_reduction
        cave_in_chance -= buff_cavein_reduction
        cave_in_chance -= stat_effects["cave_in_reduction"]
        # Lantern: -50% cave-in chance for this dig
        if has_lantern:
            cave_in_chance *= 0.50
        cave_in_chance *= relic_cavein_mod
        cave_in_chance = max(0.01, cave_in_chance)

        # Mutation: thick_skin — first cave-in each day prevented
        thick_skin_saved = False
        if mutation_fx.get("daily_cave_in_shield"):
            shield_date = tunnel.get("thick_skin_date")
            if shield_date != today:
                cave_in_chance = 0.0
                thick_skin_saved = True

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
            # Weather: cap on block loss (e.g. Mudslide Warning)
            weather_loss_cap = weather_fx.get("cave_in_loss_cap")
            if weather_loss_cap is not None:
                block_loss = min(block_loss, int(weather_loss_cap))
            # Weather: extra block loss
            block_loss += int(weather_fx.get("cave_in_loss_bonus", 0))
            # Mutation: brittle_walls — extra block loss
            block_loss += int(mutation_fx.get("cave_in_loss_bonus", 0))
            # Grappling hook prevents block loss
            if has_grappling_hook:
                block_loss = 0
            # Void-Touched pickaxe: salvage 1 block on cave-in
            elif pickaxe_tier >= 6:
                block_loss = max(1, block_loss - 1)
            new_depth = max(0, depth_before - block_loss)

            # Mutation: thick_skin — record shield used
            if thick_skin_saved:
                self.dig_repo.update_tunnel(discord_id, guild_id, thick_skin_date=today)

            # Mutation: cave_in_loot — chance to drop JC on cave-in
            cave_in_jc = 0
            loot_chance = mutation_fx.get("cave_in_loot_chance", 0)
            if loot_chance > 0 and random.random() < loot_chance:
                loot_min = int(mutation_fx.get("cave_in_loot_min", 1))
                loot_max = int(mutation_fx.get("cave_in_loot_max", 3))
                cave_in_jc = random.randint(loot_min, loot_max)
                self.player_repo.add_balance(discord_id, guild_id, cave_in_jc)

            # Mutation: second_wind — flag for next dig advance bonus
            if mutation_fx.get("post_cave_in_advance"):
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    temp_buffs=json.dumps({
                        "id": "second_wind", "name": "Second Wind",
                        "digs_remaining": 1,
                        "effect": {"advance_bonus": int(mutation_fx["post_cave_in_advance"])},
                    }),
                )

            # Random additional consequence
            # Mutation: fragile — injuries last longer
            injury_bonus = int(mutation_fx.get("injury_duration_bonus", 0))
            consequence_roll = random.random()
            if consequence_roll < 0.3:
                # Stun: extra cooldown on next dig
                cave_in_detail = {
                    "type": "stun",
                    "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're stunned.",
                }
                injury = {"type": "slower_cooldown", "digs_remaining": 2 + injury_bonus}
                self.dig_repo.update_tunnel(
                    discord_id, guild_id, injury_state=json.dumps(injury)
                )
            elif consequence_roll < 0.6:
                # Injury: reduced advance
                cave_in_detail = {
                    "type": "injury",
                    "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're injured (reduced digging for {3 + injury_bonus} digs).",
                }
                injury = {"type": "reduced_advance", "digs_remaining": 3 + injury_bonus}
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
                items_used_ids=items_used_ids,
                pickaxe_tier=pickaxe_tier,
                tip=self._pick_tip(new_depth),
                decay_info=decay_info,
                luminosity_info=lum_info,
                weather=weather_info,
            )

        # 11. Roll advance (no cave-in) — with ascension/corruption/mutation
        base_min = layer.get("advance_min", 1)
        base_max = layer.get("advance_max", 5)
        base_min += stat_effects["advance_min_bonus"]
        base_max += stat_effects["advance_max_bonus"]
        # the_endless perk: The Hollow advance becomes 1-2 instead of 1-1
        if "the_endless" in perks and layer_name == "The Hollow" and base_max <= 1:
            base_max = 2
        # Mutation: heavy_air reduces max advance
        base_max = max(base_min, base_max - int(mutation_fx.get("advance_max_penalty", 0)))
        # Corruption: min_advance_roll — roll twice take lower
        if corruption and corruption["effects"].get("min_advance_roll"):
            roll1 = random.randint(base_min, base_max)
            roll2 = random.randint(base_min, base_max)
            advance = min(roll1, roll2)
        else:
            advance = random.randint(base_min, base_max)

        # Apply modifiers
        advance += pickaxe_advance_bonus + mole_claws_bonus + buff_advance_bonus
        # Weather advance modifier
        advance += int(weather_fx.get("advance_bonus", 0))
        # Ascension advance penalty
        advance -= int(ascension.get("advance_penalty", 0))
        # Corruption advance penalty (one-dig)
        if corruption:
            advance -= int(corruption["effects"].get("advance_penalty", 0))
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

        # 13. Roll JC loot (with ascension/corruption/mutation)
        jc_min = layer.get("jc_min", 1)
        jc_max = layer.get("jc_max", 3)
        jc_earned = random.randint(jc_min, jc_max)
        # Ascension JC multiplier + weather JC multiplier
        jc_mult = 1.0 + perk_loot_bonus + ascension.get("jc_multiplier", 0) + weather_fx.get("jc_multiplier", 0)
        jc_earned = int(jc_earned * jc_mult * self._luminosity_jc_multiplier(luminosity)) + magma_heart_bonus
        # Weather: flat JC bonus/penalty
        jc_earned += int(weather_fx.get("jc_bonus", 0))
        # Corruption: fixed JC override
        if corruption and corruption["effects"].get("fixed_jc") is not None:
            jc_earned = corruption["effects"]["fixed_jc"]
        # Corruption: double-half JC (lose 1 on odd amounts)
        elif corruption and corruption["effects"].get("double_half_jc"):
            jc_earned = max(0, jc_earned - (jc_earned % 2))  # odd numbers lose 1
        # Corruption: JC penalty
        elif corruption:
            jc_earned -= int(corruption["effects"].get("jc_penalty", 0))
        # Mutation: jinxed — 5% chance 0 JC
        if mutation_fx.get("zero_jc_chance") and random.random() < mutation_fx["zero_jc_chance"]:
            jc_earned = 0
        else:
            jc_earned = max(0, jc_earned)

        # 14. Check milestones (with ascension milestone multiplier)
        milestone_bonus = 0
        milestone_mult = 1.0 + ascension.get("milestone_multiplier", 0)
        for m_depth, m_reward in MILESTONES.items():
            if depth_before < m_depth <= new_depth:
                milestone_bonus += int(m_reward * milestone_mult)

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

        # 16. Roll for artifact (skip if corruption says so)
        artifact = None
        if not (corruption and corruption["effects"].get("skip_artifact")):
            artifact = self.roll_artifact(
                discord_id, guild_id, new_depth,
                extra_rate_mod=weather_fx.get("artifact_multiplier", 1.0),
            )

        # 17. Roll for random event (layer-specific rates, luminosity, ascension, mutations)
        event_rates = {
            "Dirt": 0.16, "Stone": 0.16, "Crystal": 0.20, "Magma": 0.20,
            "Abyss": 0.24, "Fungal Depths": 0.30, "Frozen Core": 0.24, "The Hollow": 0.36,
        }
        event_chance = event_rates.get(layer_name, 0.20)
        # Ascension event chance boost
        event_chance *= (1.0 + ascension.get("event_chance_multiplier", 0))
        # Weather event chance modifier
        event_chance *= (1.0 + weather_fx.get("event_chance_multiplier", 0))
        # Mutation event_magnet boost
        event_chance *= (1.0 + mutation_fx.get("event_chance_bonus", 0))
        # Darkness increases event chance (tiered)
        if luminosity <= LUMINOSITY_PITCH_BLACK:
            event_chance *= LUMINOSITY_PITCH_EVENT_MULTIPLIER
        elif luminosity < LUMINOSITY_DIM:
            event_chance *= LUMINOSITY_DARK_EVENT_MULTIPLIER
        elif luminosity < LUMINOSITY_BRIGHT:
            event_chance *= LUMINOSITY_DIM_EVENT_MULTIPLIER
        # Void Bait: double event chance while charges remain
        void_bait_digs = tunnel.get("void_bait_digs", 0) or 0
        if void_bait_digs > 0:
            event_chance *= 2.0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                void_bait_digs=void_bait_digs - 1,
            )
        event_chance = min(event_chance, 0.75)
        # Admin force-event override
        force_key = (discord_id, guild_id)
        if hasattr(self, "_force_event_for") and force_key in self._force_event_for:
            event_chance = 1.0
            self._force_event_for.discard(force_key)
        event = None
        if random.random() < event_chance:
            event = self.roll_event(new_depth, luminosity=luminosity,
                                     prestige_level=prestige_level)

        # Sonar Pulse: preview what the next event would be
        event_preview = None
        if has_sonar_pulse:
            preview = self.roll_event(new_depth, luminosity=luminosity,
                                      prestige_level=prestige_level)
            if preview:
                event_preview = {
                    "name": preview.get("name"),
                    "description": preview.get("description"),
                    "rarity": preview.get("rarity", "common"),
                }

        # 18. Check achievements
        total_digs = (tunnel.get("total_digs", 0) or 0) + 1
        tunnel_updated = {**tunnel, "depth": new_depth, "total_digs": total_digs, "streak_days": streak}
        achievements = self.check_achievements(
            discord_id, guild_id, tunnel_updated,
            {"action": "dig", "advance": advance, "boss_encounter": boss_encounter},
        )

        # 19. Update tunnel in DB (including run counters)
        run_jc = (tunnel.get("current_run_jc", 0) or 0) + jc_earned
        run_artifacts = (tunnel.get("current_run_artifacts", 0) or 0) + (1 if artifact else 0)
        run_events_count = (tunnel.get("current_run_events", 0) or 0) + (1 if event else 0)
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=new_depth,
            total_digs=total_digs,
            last_dig_at=now,
            total_jc_earned=(tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
            streak_days=streak,
            streak_last_date=today,
            current_run_jc=run_jc,
            current_run_artifacts=run_artifacts,
            current_run_events=run_events_count,
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
                "corruption": corruption["id"] if corruption else None,
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
            items_used_ids=items_used_ids,
            pickaxe_tier=pickaxe_tier,
            tip=self._pick_tip(new_depth),
            decay_info=decay_info,
            luminosity_info=lum_info,
            paid_cost=paid_dig_cost if paid_dig_cost > 0 else 0,
            dynamite_bonus=dynamite_bonus,
            corruption=corruption,
            mutations=[m.get("name") for m in mutations] if mutations else None,
            event_preview=event_preview,
            weather=weather_info,
        )

    # ------------------------------------------------------------------
    # DM Mode: Preconditions / Outcome split
    # ------------------------------------------------------------------

    def _compute_preconditions(
        self, discord_id: int, guild_id, paid: bool = False,
    ) -> tuple[dict | None, dict | None]:
        """Compute all preconditions for a dig without rolling outcomes.

        Returns ``(terminal_result, preconditions)``.
        Exactly one of the two will be non-None.

        *terminal_result* is returned for early-exit scenarios (error,
        cooldown offer, first dig, boss-parked).

        *preconditions* is a dict with computed modifiers + effective ranges
        that the DM (or the deterministic fallback) uses to decide the outcome.
        """
        if not self.player_repo.exists(discord_id, guild_id):
            return self._error("You need to register first. Use /player register."), None

        now = int(time.time())
        today = self._get_game_date()

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        is_first_dig = False
        if tunnel is None:
            name = self.generate_tunnel_name()
            self.dig_repo.create_tunnel(discord_id, guild_id, name=name)
            tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
            is_first_dig = True
        elif self._is_unstarted_tunnel(dict(tunnel)):
            is_first_dig = True
        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id

        decay_info = self._apply_lazy_decay(tunnel, guild_id)
        depth_before = tunnel.get("depth", 0)

        if not is_first_dig:
            parked_return = self._build_parked_boss_return(
                tunnel, discord_id, guild_id, decay_info,
            )
            if parked_return is not None:
                return parked_return, None

        # Cooldown / paid dig check
        paid_dig_cost = 0
        if not is_first_dig:
            cooldown_remaining = self._get_cooldown_remaining(tunnel)
            if cooldown_remaining > 0:
                if not paid:
                    pd = tunnel.get("paid_dig_date")
                    pc = tunnel.get("paid_digs_today") or 0
                    if pd != today:
                        pc = 0
                    preview_cost = self._calculate_paid_dig_cost(tunnel, pc)
                    return {
                        "success": False,
                        "error": f"Dig on cooldown ({cooldown_remaining}s remaining).",
                        "cooldown_remaining": cooldown_remaining,
                        "paid_dig_cost": preview_cost,
                        "paid_dig_available": True,
                    }, None

                paid_date = tunnel.get("paid_dig_date")
                paid_count = tunnel.get("paid_digs_today") or 0
                if paid_date != today:
                    paid_count = 0
                paid_dig_cost = self._calculate_paid_dig_cost(tunnel, paid_count)
                balance = self.player_repo.get_balance(discord_id, guild_id)
                if balance < paid_dig_cost:
                    return self._error(
                        f"Paid dig costs {paid_dig_cost} JC but you only have {balance} JC."
                    ), None
                self.player_repo.add_balance(discord_id, guild_id, -paid_dig_cost)
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    paid_dig_date=today,
                    paid_digs_today=paid_count + 1,
                )

        if is_first_dig:
            return self._execute_first_dig(
                discord_id, guild_id, tunnel, depth_before, now, today, decay_info,
            ), None

        # Injury state
        injury_advance_mod = 1.0
        if tunnel.get("injury_state"):
            try:
                injury = json.loads(tunnel["injury_state"])
            except (json.JSONDecodeError, TypeError):
                injury = None
            else:
                if injury and injury.get("digs_remaining", 0) > 0:
                    if injury.get("type") == "reduced_advance":
                        injury_advance_mod = 0.5
                    injury["digs_remaining"] -= 1
                    if injury["digs_remaining"] <= 0:
                        injury = None
                    self.dig_repo.update_tunnel(
                        discord_id, guild_id,
                        injury_state=json.dumps(injury) if injury else None,
                    )

        # Queued items
        items_used, items_used_ids, _item_flags = self._resolve_queued_items(
            discord_id, guild_id,
        )
        has_dynamite = _item_flags["has_dynamite"]
        has_hard_hat = _item_flags["has_hard_hat"]
        has_lantern = _item_flags["has_lantern"]
        has_torch = _item_flags["has_torch"]
        has_grappling_hook = _item_flags["has_grappling_hook"]
        has_depth_charge = _item_flags["has_depth_charge"]
        has_reinforcement = _item_flags["has_reinforcement"]
        has_sonar_pulse = _item_flags["has_sonar_pulse"]
        has_void_bait = _item_flags["has_void_bait"]

        if has_hard_hat:
            existing_charges = tunnel.get("hard_hat_charges", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id, hard_hat_charges=existing_charges + 3,
            )
            tunnel["hard_hat_charges"] = existing_charges + 3
        if has_reinforcement:
            self.dig_repo.update_tunnel(
                discord_id, guild_id, reinforced_until=now + 48 * 3600,
            )
        if has_void_bait:
            existing_vb = tunnel.get("void_bait_digs", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id, void_bait_digs=existing_vb + 3,
            )
            tunnel["void_bait_digs"] = existing_vb + 3

        # Layer, luminosity, weather, buffs
        layer = self._get_layer(depth_before)
        layer_name = layer.get("name", "Dirt")
        weather_fx = self._get_weather_effects(guild_id, layer_name)
        weather_info = None
        if weather_fx:
            for entry in self._ensure_weather(guild_id):
                if entry.get("layer_name") == layer_name:
                    w = WEATHER_BY_ID.get(entry.get("weather_id"))
                    if w:
                        weather_info = {"name": w.name, "description": w.description}

        lum_info = self._apply_luminosity_drain(discord_id, guild_id, tunnel, layer_name)
        luminosity = lum_info["luminosity_after"]

        if has_torch:
            luminosity = min(LUMINOSITY_MAX, luminosity + 50)
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)
            lum_info["luminosity_after"] = luminosity

        if self._has_relic(discord_id, guild_id, "spore_cloak") and lum_info["drained"] > 0:
            restored = lum_info["drained"] // 2
            luminosity = min(LUMINOSITY_MAX, luminosity + restored)
            lum_info["drained"] -= restored
            lum_info["luminosity_after"] = luminosity
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

        active_buff = self._get_active_buff(tunnel)
        buff_effects = self._apply_buff_effects(active_buff)
        buff_advance_bonus = buff_effects.get("advance_bonus", 0)
        buff_cavein_reduction = buff_effects.get("cave_in_reduction", 0.0)
        self._decrement_buff(discord_id, guild_id, tunnel)

        # Prestige, ascension, corruption, mutations, pickaxe
        perks = self._get_prestige_perks(tunnel)
        prestige_level = tunnel.get("prestige_level", 0) or 0
        ascension = self._get_ascension_effects(prestige_level)
        corruption = self._roll_corruption(prestige_level)
        mutations = self._get_mutations(tunnel)
        mutation_fx = self._apply_mutation_effects(mutations)

        extra_drain = ascension.get("luminosity_drain_multiplier", 0)
        if extra_drain > 0 and lum_info["drained"] > 0:
            bonus_drain = int(lum_info["drained"] * extra_drain)
            luminosity = max(0, luminosity - bonus_drain)
            lum_info["luminosity_after"] = luminosity
            lum_info["drained"] += bonus_drain
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

        weather_drain = weather_fx.get("luminosity_drain_multiplier", 0)
        if weather_drain > 0 and lum_info["drained"] > 0:
            bonus_drain = int(lum_info["drained"] * weather_drain)
            luminosity = max(0, luminosity - bonus_drain)
            lum_info["luminosity_after"] = luminosity
            lum_info["drained"] += bonus_drain
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

        pickaxe_tier = tunnel.get("pickaxe_tier", 0) or 0
        pickaxe_data = PICKAXE_TIERS[pickaxe_tier] if pickaxe_tier < len(PICKAXE_TIERS) else {}
        pickaxe_advance_bonus = pickaxe_data.get("advance_bonus", 0)
        pickaxe_cavein_reduction = pickaxe_data.get("cave_in_reduction", 0)

        perk_cavein_reduction = 0.05 if "reinforced_walls" in perks else 0.0
        perk_advance_bonus = 0.1 if "efficient_digging" in perks else 0.0
        perk_loot_bonus = 0.15 if "keen_eye" in perks else 0.0

        if "deep_sight" in perks and lum_info.get("drained", 0) > 0:
            restored = max(1, lum_info["drained"] // 4)
            luminosity = min(LUMINOSITY_MAX, luminosity + restored)
            lum_info["luminosity_after"] = luminosity
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)
            tunnel["luminosity"] = luminosity

        relic_cavein_mod = 0.97 if self._has_relic(discord_id, guild_id, "crystal_compass") else 1.0
        mole_claws_bonus = 1 if self._has_relic(discord_id, guild_id, "mole_claws") else 0
        magma_heart_bonus = 1 if self._has_relic(discord_id, guild_id, "magma_heart") else 0
        miner_stats = self._get_miner_stats(tunnel)
        stat_effects = self._get_stat_effects(miner_stats)

        # ── Cave-in chance ────────────────────────────────────────
        hard_hat_charges = tunnel.get("hard_hat_charges", 0) or 0
        cave_in_chance = layer.get("cave_in_pct", 0.10)
        cave_in_chance += ascension.get("cave_in_bonus", 0)
        cave_in_chance += weather_fx.get("cave_in_bonus", 0)
        if corruption:
            cave_in_chance += corruption["effects"].get("cave_in_bonus", 0)
        lum_cave_bonus = self._luminosity_cave_in_bonus(luminosity)
        if "dark_adaptation" in perks and LUMINOSITY_DIM <= luminosity < LUMINOSITY_BRIGHT:
            lum_cave_bonus = 0.0
        if mutation_fx.get("ignore_luminosity_cave_in"):
            lum_cave_bonus = 0.0
        cave_in_chance += lum_cave_bonus
        cave_in_chance -= perk_cavein_reduction
        cave_in_chance -= pickaxe_cavein_reduction
        cave_in_chance -= buff_cavein_reduction
        cave_in_chance -= stat_effects["cave_in_reduction"]
        if has_lantern:
            cave_in_chance *= 0.50
        cave_in_chance *= relic_cavein_mod
        cave_in_chance = max(0.01, cave_in_chance)

        thick_skin_saved = False
        if mutation_fx.get("daily_cave_in_shield"):
            shield_date = tunnel.get("thick_skin_date")
            if shield_date != today:
                cave_in_chance = 0.0
                thick_skin_saved = True

        hard_hat_prevents = hard_hat_charges > 0

        # ── Effective advance range ───────────────────────────────
        base_adv_min = layer.get("advance_min", 1)
        base_adv_max = layer.get("advance_max", 5)
        base_adv_min += stat_effects["advance_min_bonus"]
        base_adv_max += stat_effects["advance_max_bonus"]
        if "the_endless" in perks and layer_name == "The Hollow" and base_adv_max <= 1:
            base_adv_max = 2
        base_adv_max = max(
            base_adv_min,
            base_adv_max - int(mutation_fx.get("advance_max_penalty", 0)),
        )

        adv_fixed = pickaxe_advance_bonus + mole_claws_bonus + buff_advance_bonus
        adv_fixed += int(weather_fx.get("advance_bonus", 0))
        adv_fixed -= int(ascension.get("advance_penalty", 0))
        if corruption:
            adv_fixed -= int(corruption["effects"].get("advance_penalty", 0))
        if has_dynamite:
            adv_fixed += 5
        if has_depth_charge:
            adv_fixed += 8

        adv_mult = (1.0 + perk_advance_bonus) * injury_advance_mod
        advance_min = max(1, int((base_adv_min + adv_fixed) * adv_mult))
        advance_max = max(1, int((base_adv_max + adv_fixed) * adv_mult))
        if has_depth_charge:
            advance_min = max(1, advance_min - 3)
            advance_max = max(1, advance_max - 3)

        # ── Effective JC range ────────────────────────────────────
        jc_min_base = layer.get("jc_min", 1)
        jc_max_base = layer.get("jc_max", 3)
        jc_mult = (
            1.0
            + perk_loot_bonus
            + ascension.get("jc_multiplier", 0)
            + weather_fx.get("jc_multiplier", 0)
        )
        jc_mult *= self._luminosity_jc_multiplier(luminosity)
        jc_fixed = magma_heart_bonus + int(weather_fx.get("jc_bonus", 0))
        jc_min = max(0, int(jc_min_base * jc_mult) + jc_fixed)
        jc_max = max(0, int(jc_max_base * jc_mult) + jc_fixed)
        if corruption and corruption["effects"].get("fixed_jc") is not None:
            jc_min = jc_max = corruption["effects"]["fixed_jc"]

        # ── Event chance + eligible events ────────────────────────
        event_rates = {
            "Dirt": 0.16, "Stone": 0.16, "Crystal": 0.20, "Magma": 0.20,
            "Abyss": 0.24, "Fungal Depths": 0.30, "Frozen Core": 0.24,
            "The Hollow": 0.36,
        }
        event_chance = event_rates.get(layer_name, 0.20)
        event_chance *= 1.0 + ascension.get("event_chance_multiplier", 0)
        event_chance *= 1.0 + weather_fx.get("event_chance_multiplier", 0)
        event_chance *= 1.0 + mutation_fx.get("event_chance_bonus", 0)
        if luminosity <= LUMINOSITY_PITCH_BLACK:
            event_chance *= LUMINOSITY_PITCH_EVENT_MULTIPLIER
        elif luminosity < LUMINOSITY_DIM:
            event_chance *= LUMINOSITY_DARK_EVENT_MULTIPLIER
        elif luminosity < LUMINOSITY_BRIGHT:
            event_chance *= LUMINOSITY_DIM_EVENT_MULTIPLIER
        void_bait_digs = tunnel.get("void_bait_digs", 0) or 0
        if void_bait_digs > 0:
            event_chance *= 2.0
        event_chance = min(event_chance, 0.75)
        force_key = (discord_id, guild_id)
        if hasattr(self, "_force_event_for") and force_key in self._force_event_for:
            event_chance = 1.0
            self._force_event_for.discard(force_key)

        is_pitch_black = luminosity <= 0
        art_ids = _get_events_with_art()
        available_events = [
            {
                "id": e["id"],
                "name": e["name"],
                "rarity": e.get("rarity", "common"),
                "has_art": e["id"] in art_ids,
            }
            for e in EVENT_POOL
            if depth_before >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth_before <= e["max_depth"])
            and (e.get("layer") is None or e["layer"] == layer_name)
            and (not e.get("requires_dark") or is_pitch_black)
            and prestige_level >= e.get("min_prestige", 0)
        ]

        preconditions = {
            "discord_id": discord_id,
            "guild_id": guild_id,
            "now": now,
            "today": today,
            "tunnel": tunnel,
            "depth_before": depth_before,
            "decay_info": decay_info,
            "injury_advance_mod": injury_advance_mod,
            "items_used": items_used,
            "items_used_ids": items_used_ids,
            "has_dynamite": has_dynamite,
            "has_hard_hat": has_hard_hat,
            "has_lantern": has_lantern,
            "has_grappling_hook": has_grappling_hook,
            "has_depth_charge": has_depth_charge,
            "has_sonar_pulse": has_sonar_pulse,
            "layer": layer,
            "layer_name": layer_name,
            "luminosity": luminosity,
            "lum_info": lum_info,
            "weather_fx": weather_fx,
            "weather_info": weather_info,
            "buff_advance_bonus": buff_advance_bonus,
            "buff_cavein_reduction": buff_cavein_reduction,
            "perks": perks,
            "prestige_level": prestige_level,
            "ascension": ascension,
            "corruption": corruption,
            "mutations": mutations,
            "mutation_fx": mutation_fx,
            "pickaxe_tier": pickaxe_tier,
            "pickaxe_advance_bonus": pickaxe_advance_bonus,
            "perk_advance_bonus": perk_advance_bonus,
            "perk_loot_bonus": perk_loot_bonus,
            "mole_claws_bonus": mole_claws_bonus,
            "magma_heart_bonus": magma_heart_bonus,
            "miner_stats": miner_stats,
            "stat_effects": stat_effects,
            "hard_hat_charges": hard_hat_charges,
            "hard_hat_prevents": hard_hat_prevents,
            "cave_in_chance": cave_in_chance,
            "thick_skin_saved": thick_skin_saved,
            "paid_dig_cost": paid_dig_cost,
            "advance_min": advance_min,
            "advance_max": advance_max,
            "jc_min": jc_min,
            "jc_max": jc_max,
            "event_chance": event_chance,
            "available_events": available_events,
        }
        return None, preconditions

    def dig_with_preconditions(
        self, discord_id: int, guild_id, paid: bool = False,
    ) -> tuple[dict | None, dict | None]:
        """Public interface for DM mode: compute preconditions only.

        Returns ``(terminal_result, preconditions)``.
        If *terminal_result* is not None the dig ends there (error / cooldown /
        first-dig / boss-parked).  Otherwise *preconditions* has the computed
        state the DM uses to decide the outcome.
        """
        return self._compute_preconditions(discord_id, guild_id, paid)

    def _execute_deterministic_outcome(self, p: dict) -> dict:
        """Run the deterministic outcome phase on pre-computed preconditions.

        This is the fallback path when the DM is unavailable.  It mirrors
        steps 9-22 of the original ``dig()`` method.
        """
        discord_id = p["discord_id"]
        guild_id = p["guild_id"]
        now = p["now"]
        today = p["today"]
        tunnel = p["tunnel"]
        depth_before = p["depth_before"]

        # Cave-in check
        if p["hard_hat_prevents"]:
            cave_in = False
            self.dig_repo.update_tunnel(
                discord_id, guild_id, hard_hat_charges=p["hard_hat_charges"] - 1,
            )
        else:
            cave_in = random.random() < p["cave_in_chance"]
        cave_in_detail = None

        if cave_in:
            block_loss = random.randint(3, 8)
            weather_loss_cap = p["weather_fx"].get("cave_in_loss_cap")
            if weather_loss_cap is not None:
                block_loss = min(block_loss, int(weather_loss_cap))
            block_loss += int(p["weather_fx"].get("cave_in_loss_bonus", 0))
            block_loss += int(p["mutation_fx"].get("cave_in_loss_bonus", 0))
            if p["has_grappling_hook"]:
                block_loss = 0
            elif p["pickaxe_tier"] >= 6:
                block_loss = max(1, block_loss - 1)
            new_depth = max(0, depth_before - block_loss)

            if p["thick_skin_saved"]:
                self.dig_repo.update_tunnel(discord_id, guild_id, thick_skin_date=today)

            cave_in_jc = 0
            loot_chance = p["mutation_fx"].get("cave_in_loot_chance", 0)
            if loot_chance > 0 and random.random() < loot_chance:
                loot_min = int(p["mutation_fx"].get("cave_in_loot_min", 1))
                loot_max = int(p["mutation_fx"].get("cave_in_loot_max", 3))
                cave_in_jc = random.randint(loot_min, loot_max)
                self.player_repo.add_balance(discord_id, guild_id, cave_in_jc)

            if p["mutation_fx"].get("post_cave_in_advance"):
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    temp_buffs=json.dumps({
                        "id": "second_wind", "name": "Second Wind",
                        "digs_remaining": 1,
                        "effect": {"advance_bonus": int(p["mutation_fx"]["post_cave_in_advance"])},
                    }),
                )

            injury_bonus = int(p["mutation_fx"].get("injury_duration_bonus", 0))
            consequence_roll = random.random()
            if consequence_roll < 0.3:
                cave_in_detail = {
                    "type": "stun", "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're stunned.",
                }
                injury = {"type": "slower_cooldown", "digs_remaining": 2 + injury_bonus}
                self.dig_repo.update_tunnel(discord_id, guild_id, injury_state=json.dumps(injury))
            elif consequence_roll < 0.6:
                cave_in_detail = {
                    "type": "injury", "block_loss": block_loss,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks and you're injured "
                        f"(reduced digging for {3 + injury_bonus} digs)."
                    ),
                }
                injury = {"type": "reduced_advance", "digs_remaining": 3 + injury_bonus}
                self.dig_repo.update_tunnel(discord_id, guild_id, injury_state=json.dumps(injury))
            else:
                med_cost = random.randint(2, 6)
                balance = self.player_repo.get_balance(discord_id, guild_id)
                med_cost = min(med_cost, max(0, balance))
                if med_cost > 0:
                    self.player_repo.add_balance(discord_id, guild_id, -med_cost)
                cave_in_detail = {
                    "type": "medical_bill", "block_loss": block_loss,
                    "jc_lost": med_cost,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks and paid "
                        f"{med_cost} JC in medical bills."
                    ),
                }

            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                total_digs=(tunnel.get("total_digs", 0) or 0) + 1,
                last_dig_at=now,
            )
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
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
                depth_before=depth_before, depth_after=new_depth,
                advance=0, jc_earned=0, milestone_bonus=0, streak_bonus=0,
                cave_in=True, cave_in_detail=cave_in_detail,
                boss_encounter=False, boss_info=None,
                has_lantern=p["has_lantern"],
                event=None, artifact=None,
                achievements=achievements, is_first_dig=False,
                items_used=p["items_used"], items_used_ids=p["items_used_ids"],
                pickaxe_tier=p["pickaxe_tier"],
                tip=self._pick_tip(new_depth),
                decay_info=p["decay_info"],
                luminosity_info=p["lum_info"],
                weather=p["weather_info"],
            )

        # No cave-in — roll advance
        layer = p["layer"]
        layer_name = p["layer_name"]
        base_min = layer.get("advance_min", 1)
        base_max = layer.get("advance_max", 5)
        stat_effects = p.get("stat_effects", {})
        base_min += int(stat_effects.get("advance_min_bonus", 0))
        base_max += int(stat_effects.get("advance_max_bonus", 0))
        if "the_endless" in p["perks"] and layer_name == "The Hollow" and base_max <= 1:
            base_max = 2
        base_max = max(base_min, base_max - int(p["mutation_fx"].get("advance_max_penalty", 0)))
        if p["corruption"] and p["corruption"]["effects"].get("min_advance_roll"):
            roll1 = random.randint(base_min, base_max)
            roll2 = random.randint(base_min, base_max)
            advance = min(roll1, roll2)
        else:
            advance = random.randint(base_min, base_max)

        advance += p["pickaxe_advance_bonus"] + p["mole_claws_bonus"] + p["buff_advance_bonus"]
        advance += int(p["weather_fx"].get("advance_bonus", 0))
        advance -= int(p["ascension"].get("advance_penalty", 0))
        if p["corruption"]:
            advance -= int(p["corruption"]["effects"].get("advance_penalty", 0))
        dynamite_bonus = 0
        if p["has_dynamite"]:
            dynamite_bonus = 5
            advance += dynamite_bonus
        depth_charge_bonus = 0
        if p["has_depth_charge"]:
            depth_charge_bonus = 8
            advance += depth_charge_bonus
        advance = int(
            advance * (1.0 + p["perk_advance_bonus"]) * p["injury_advance_mod"]
        )
        advance = max(1, advance)
        if p["has_depth_charge"]:
            advance = max(1, advance - 3)

        # Boss boundary
        boss_progress = self._get_boss_progress(tunnel)
        next_boss = self._next_boss_boundary(depth_before, boss_progress)
        boss_encounter = False
        boss_info = None
        if next_boss is not None and depth_before + advance >= next_boss:
            advance = max(0, next_boss - 1 - depth_before)
            boss_encounter = True
            boss_name = BOSS_NAMES.get(next_boss, "Unknown Boss")
            attempts = tunnel.get("boss_attempts", 0) or 0
            dialogue_list = BOSS_DIALOGUE.get(next_boss, ["..."])
            boss_info = {
                "boundary": next_boss, "name": boss_name,
                "dialogue": dialogue_list[min(attempts, len(dialogue_list) - 1)],
                "ascii_art": BOSS_ASCII.get(next_boss, ""),
            }
        new_depth = depth_before + advance

        # JC loot
        luminosity = p["luminosity"]
        jc_min_base = layer.get("jc_min", 1)
        jc_max_base = layer.get("jc_max", 3)
        jc_earned = random.randint(jc_min_base, jc_max_base)
        jc_mult = (
            1.0
            + p["perk_loot_bonus"]
            + p["ascension"].get("jc_multiplier", 0)
            + p["weather_fx"].get("jc_multiplier", 0)
        )
        jc_earned = (
            int(jc_earned * jc_mult * self._luminosity_jc_multiplier(luminosity))
            + p["magma_heart_bonus"]
        )
        jc_earned += int(p["weather_fx"].get("jc_bonus", 0))
        if p["corruption"] and p["corruption"]["effects"].get("fixed_jc") is not None:
            jc_earned = p["corruption"]["effects"]["fixed_jc"]
        elif p["corruption"] and p["corruption"]["effects"].get("double_half_jc"):
            jc_earned = max(0, jc_earned - (jc_earned % 2))
        elif p["corruption"]:
            jc_earned -= int(p["corruption"]["effects"].get("jc_penalty", 0))
        if p["mutation_fx"].get("zero_jc_chance") and random.random() < p["mutation_fx"]["zero_jc_chance"]:
            jc_earned = 0
        else:
            jc_earned = max(0, jc_earned)

        # Milestones
        milestone_bonus = 0
        milestone_mult = 1.0 + p["ascension"].get("milestone_multiplier", 0)
        for m_depth, m_reward in MILESTONES.items():
            if depth_before < m_depth <= new_depth:
                milestone_bonus += int(m_reward * milestone_mult)
        jc_earned += milestone_bonus

        # Streak
        streak = tunnel.get("streak_days", 0) or 0
        streak_last = tunnel.get("streak_last_date")
        yesterday = (
            datetime.datetime.strptime(today, "%Y-%m-%d") - datetime.timedelta(days=1)
        ).strftime("%Y-%m-%d")
        if streak_last == yesterday:
            streak += 1
        elif streak_last == today:
            pass
        else:
            streak = 1
        streak_bonus = 0
        for threshold in sorted(STREAKS.keys(), reverse=True):
            if streak >= threshold:
                streak_bonus = STREAKS[threshold]
                break
        jc_earned += streak_bonus

        # Artifact
        artifact = None
        if not (p["corruption"] and p["corruption"]["effects"].get("skip_artifact")):
            artifact = self.roll_artifact(
                discord_id, guild_id, new_depth,
                extra_rate_mod=p["weather_fx"].get("artifact_multiplier", 1.0),
            )

        # Event
        void_bait_digs = tunnel.get("void_bait_digs", 0) or 0
        if void_bait_digs > 0:
            self.dig_repo.update_tunnel(
                discord_id, guild_id, void_bait_digs=void_bait_digs - 1,
            )
        event = None
        if random.random() < p["event_chance"]:
            event = self.roll_event(
                new_depth, luminosity=luminosity, prestige_level=p["prestige_level"],
            )

        event_preview = None
        if p["has_sonar_pulse"]:
            preview = self.roll_event(
                new_depth, luminosity=luminosity, prestige_level=p["prestige_level"],
            )
            if preview:
                event_preview = {
                    "name": preview.get("name"),
                    "description": preview.get("description"),
                    "rarity": preview.get("rarity", "common"),
                }

        # Achievements
        total_digs = (tunnel.get("total_digs", 0) or 0) + 1
        tunnel_updated = {
            **tunnel, "depth": new_depth, "total_digs": total_digs, "streak_days": streak,
        }
        achievements = self.check_achievements(
            discord_id, guild_id, tunnel_updated,
            {"action": "dig", "advance": advance, "boss_encounter": boss_encounter},
        )

        # DB writes
        run_jc = (tunnel.get("current_run_jc", 0) or 0) + jc_earned
        run_artifacts = (tunnel.get("current_run_artifacts", 0) or 0) + (1 if artifact else 0)
        run_events_count = (tunnel.get("current_run_events", 0) or 0) + (1 if event else 0)
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=new_depth, total_digs=total_digs, last_dig_at=now,
            total_jc_earned=(tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
            streak_days=streak, streak_last_date=today,
            current_run_jc=run_jc,
            current_run_artifacts=run_artifacts,
            current_run_events=run_events_count,
        )
        self.player_repo.add_balance(discord_id, guild_id, jc_earned)
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id, action_type="dig",
            details=json.dumps({
                "advance": advance, "jc": jc_earned,
                "depth_before": depth_before, "depth_after": new_depth,
                "boss_encounter": boss_encounter, "cave_in": False,
                "corruption": p["corruption"]["id"] if p["corruption"] else None,
            }),
        )

        paid_dig_cost = p["paid_dig_cost"]
        return self._ok(
            tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
            depth_before=depth_before, depth_after=new_depth,
            advance=advance, jc_earned=jc_earned,
            milestone_bonus=milestone_bonus, streak_bonus=streak_bonus,
            cave_in=False, cave_in_detail=None,
            boss_encounter=boss_encounter, boss_info=boss_info,
            has_lantern=p["has_lantern"],
            event=event, artifact=artifact,
            achievements=achievements, is_first_dig=False,
            items_used=p["items_used"], items_used_ids=p["items_used_ids"],
            pickaxe_tier=p["pickaxe_tier"],
            tip=self._pick_tip(new_depth),
            decay_info=p["decay_info"],
            luminosity_info=p["lum_info"],
            paid_cost=paid_dig_cost if paid_dig_cost > 0 else 0,
            dynamite_bonus=dynamite_bonus,
            corruption=p["corruption"],
            mutations=[m.get("name") for m in p["mutations"]] if p["mutations"] else None,
            event_preview=event_preview,
            weather=p["weather_info"],
        )

    def apply_dig_outcome(self, preconditions: dict, outcome: dict) -> dict:
        """Apply a DM-decided outcome to the database.

        *outcome* should contain keys from the ``resolve_dig`` tool call:
        advance, jc_earned, cave_in, cave_in_block_loss, cave_in_type,
        cave_in_jc_lost, event_id, narrative, tone.

        Handles boss-boundary capping, milestone/streak bonuses, achievement
        checking, and all DB writes.  Returns the standard result dict for
        the embed builder.
        """
        p = preconditions
        discord_id = p["discord_id"]
        guild_id = p["guild_id"]
        now = p["now"]
        today = p["today"]
        tunnel = p["tunnel"]
        depth_before = p["depth_before"]

        cave_in = outcome.get("cave_in", False)

        # Hard hat prevents cave-in regardless of DM decision
        if p["hard_hat_prevents"]:
            cave_in = False
            self.dig_repo.update_tunnel(
                discord_id, guild_id, hard_hat_charges=p["hard_hat_charges"] - 1,
            )

        if cave_in:
            block_loss = outcome.get("cave_in_block_loss", 5)
            # Enforce game-rule constraints
            if p["has_grappling_hook"]:
                block_loss = 0
            elif p["pickaxe_tier"] >= 6:
                block_loss = max(1, block_loss - 1)
            weather_loss_cap = p["weather_fx"].get("cave_in_loss_cap")
            if weather_loss_cap is not None:
                block_loss = min(block_loss, int(weather_loss_cap))

            new_depth = max(0, depth_before - block_loss)

            if p["thick_skin_saved"]:
                self.dig_repo.update_tunnel(discord_id, guild_id, thick_skin_date=today)

            # Cave-in type from DM
            cave_in_type = outcome.get("cave_in_type", "stun")
            injury_bonus = int(p["mutation_fx"].get("injury_duration_bonus", 0))

            if cave_in_type == "stun":
                cave_in_detail = {
                    "type": "stun", "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're stunned.",
                }
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    injury_state=json.dumps(
                        {"type": "slower_cooldown", "digs_remaining": 2 + injury_bonus}
                    ),
                )
            elif cave_in_type == "injury":
                cave_in_detail = {
                    "type": "injury", "block_loss": block_loss,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks and you're injured "
                        f"(reduced digging for {3 + injury_bonus} digs)."
                    ),
                }
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    injury_state=json.dumps(
                        {"type": "reduced_advance", "digs_remaining": 3 + injury_bonus}
                    ),
                )
            else:  # medical_bill
                med_cost = outcome.get("cave_in_jc_lost", 3)
                balance = self.player_repo.get_balance(discord_id, guild_id)
                med_cost = min(med_cost, max(0, balance))
                if med_cost > 0:
                    self.player_repo.add_balance(discord_id, guild_id, -med_cost)
                cave_in_detail = {
                    "type": "medical_bill", "block_loss": block_loss,
                    "jc_lost": med_cost,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks and paid "
                        f"{med_cost} JC in medical bills."
                    ),
                }

            # Mutation: cave_in_loot
            loot_chance = p["mutation_fx"].get("cave_in_loot_chance", 0)
            if loot_chance > 0 and random.random() < loot_chance:
                loot_min = int(p["mutation_fx"].get("cave_in_loot_min", 1))
                loot_max = int(p["mutation_fx"].get("cave_in_loot_max", 3))
                self.player_repo.add_balance(
                    discord_id, guild_id, random.randint(loot_min, loot_max),
                )
            # Mutation: second_wind
            if p["mutation_fx"].get("post_cave_in_advance"):
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    temp_buffs=json.dumps({
                        "id": "second_wind", "name": "Second Wind",
                        "digs_remaining": 1,
                        "effect": {"advance_bonus": int(p["mutation_fx"]["post_cave_in_advance"])},
                    }),
                )

            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                total_digs=(tunnel.get("total_digs", 0) or 0) + 1,
                last_dig_at=now,
            )
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
                details=json.dumps({
                    "cave_in": True, "block_loss": block_loss,
                    "detail": cave_in_detail,
                    "depth_before": depth_before, "depth_after": new_depth,
                    "dm_mode": True,
                }),
            )
            achievements = self.check_achievements(
                discord_id, guild_id,
                {**tunnel, "depth": new_depth},
                {"action": "cave_in"},
            )
            result = self._ok(
                tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                depth_before=depth_before, depth_after=new_depth,
                advance=0, jc_earned=0, milestone_bonus=0, streak_bonus=0,
                cave_in=True, cave_in_detail=cave_in_detail,
                boss_encounter=False, boss_info=None,
                has_lantern=p["has_lantern"],
                event=None, artifact=None,
                achievements=achievements, is_first_dig=False,
                items_used=p["items_used"], items_used_ids=p["items_used_ids"],
                pickaxe_tier=p["pickaxe_tier"],
                tip=self._pick_tip(new_depth),
                decay_info=p["decay_info"],
                luminosity_info=p["lum_info"],
                weather=p["weather_info"],
            )
        else:
            # No cave-in — DM-decided advance + JC
            advance = outcome.get("advance", 1)

            # Boss boundary cap (DM cannot skip bosses)
            boss_progress = self._get_boss_progress(tunnel)
            next_boss = self._next_boss_boundary(depth_before, boss_progress)
            boss_encounter = False
            boss_info = None
            if next_boss is not None and depth_before + advance >= next_boss:
                advance = max(0, next_boss - 1 - depth_before)
                boss_encounter = True
                boss_name = BOSS_NAMES.get(next_boss, "Unknown Boss")
                attempts = tunnel.get("boss_attempts", 0) or 0
                dialogue_list = BOSS_DIALOGUE.get(next_boss, ["..."])
                boss_info = {
                    "boundary": next_boss, "name": boss_name,
                    "dialogue": dialogue_list[min(attempts, len(dialogue_list) - 1)],
                    "ascii_art": BOSS_ASCII.get(next_boss, ""),
                }
            new_depth = depth_before + advance

            jc_earned = outcome.get("jc_earned", 0)

            # Milestones (deterministic bookkeeping)
            milestone_bonus = 0
            milestone_mult = 1.0 + p["ascension"].get("milestone_multiplier", 0)
            for m_depth, m_reward in MILESTONES.items():
                if depth_before < m_depth <= new_depth:
                    milestone_bonus += int(m_reward * milestone_mult)
            jc_earned += milestone_bonus

            # Streak (deterministic bookkeeping)
            streak = tunnel.get("streak_days", 0) or 0
            streak_last = tunnel.get("streak_last_date")
            yesterday = (
                datetime.datetime.strptime(today, "%Y-%m-%d") - datetime.timedelta(days=1)
            ).strftime("%Y-%m-%d")
            if streak_last == yesterday:
                streak += 1
            elif streak_last == today:
                pass
            else:
                streak = 1
            streak_bonus = 0
            for threshold in sorted(STREAKS.keys(), reverse=True):
                if streak >= threshold:
                    streak_bonus = STREAKS[threshold]
                    break
            jc_earned += streak_bonus

            # Artifact (deterministic)
            artifact = None
            if not (p["corruption"] and p["corruption"]["effects"].get("skip_artifact")):
                artifact = self.roll_artifact(
                    discord_id, guild_id, new_depth,
                    extra_rate_mod=p["weather_fx"].get("artifact_multiplier", 1.0),
                )

            # Event from DM
            event = None
            event_id = outcome.get("event_id", "")
            if event_id:
                pool_event = next((e for e in EVENT_POOL if e["id"] == event_id), None)
                if pool_event:
                    event = {
                        "id": pool_event["id"],
                        "name": pool_event["name"],
                        "description": outcome.get("event_description") or pool_event["description"],
                        "complexity": pool_event.get("complexity", "choice"),
                        "safe_option": pool_event.get("safe_option"),
                        "risky_option": pool_event.get("risky_option"),
                        "desperate_option": pool_event.get("desperate_option"),
                        "boon_options": pool_event.get("boon_options"),
                        "buff_on_success": pool_event.get("buff_on_success"),
                        "rarity": pool_event.get("rarity", "common"),
                    }

            # Void bait decrement
            void_bait_digs = tunnel.get("void_bait_digs", 0) or 0
            if void_bait_digs > 0:
                self.dig_repo.update_tunnel(
                    discord_id, guild_id, void_bait_digs=void_bait_digs - 1,
                )

            # Sonar pulse
            event_preview = None
            if p["has_sonar_pulse"]:
                preview = self.roll_event(
                    new_depth, luminosity=p["luminosity"], prestige_level=p["prestige_level"],
                )
                if preview:
                    event_preview = {
                        "name": preview.get("name"),
                        "description": preview.get("description"),
                        "rarity": preview.get("rarity", "common"),
                    }

            # Achievements
            total_digs = (tunnel.get("total_digs", 0) or 0) + 1
            tunnel_updated = {
                **tunnel, "depth": new_depth, "total_digs": total_digs, "streak_days": streak,
            }
            achievements = self.check_achievements(
                discord_id, guild_id, tunnel_updated,
                {"action": "dig", "advance": advance, "boss_encounter": boss_encounter},
            )

            # DB writes
            run_jc = (tunnel.get("current_run_jc", 0) or 0) + jc_earned
            run_artifacts = (tunnel.get("current_run_artifacts", 0) or 0) + (1 if artifact else 0)
            run_events_count = (tunnel.get("current_run_events", 0) or 0) + (1 if event else 0)
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth, total_digs=total_digs, last_dig_at=now,
                total_jc_earned=(tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
                streak_days=streak, streak_last_date=today,
                current_run_jc=run_jc,
                current_run_artifacts=run_artifacts,
                current_run_events=run_events_count,
            )
            self.player_repo.add_balance(discord_id, guild_id, jc_earned)
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
                details=json.dumps({
                    "advance": advance, "jc": jc_earned,
                    "depth_before": depth_before, "depth_after": new_depth,
                    "boss_encounter": boss_encounter, "cave_in": False,
                    "corruption": p["corruption"]["id"] if p["corruption"] else None,
                    "dm_mode": True,
                }),
            )

            paid_dig_cost = p["paid_dig_cost"]
            result = self._ok(
                tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                depth_before=depth_before, depth_after=new_depth,
                advance=advance, jc_earned=jc_earned,
                milestone_bonus=milestone_bonus, streak_bonus=streak_bonus,
                cave_in=False, cave_in_detail=None,
                boss_encounter=boss_encounter, boss_info=boss_info,
                has_lantern=p["has_lantern"],
                event=event, artifact=artifact,
                achievements=achievements, is_first_dig=False,
                items_used=p["items_used"], items_used_ids=p["items_used_ids"],
                pickaxe_tier=p["pickaxe_tier"],
                tip=self._pick_tip(new_depth),
                decay_info=p["decay_info"],
                luminosity_info=p["lum_info"],
                paid_cost=paid_dig_cost if paid_dig_cost > 0 else 0,
                corruption=p["corruption"],
                mutations=[m.get("name") for m in p["mutations"]] if p["mutations"] else None,
                event_preview=event_preview,
                weather=p["weather_info"],
            )

        return result

    def reset_dig_cooldown(self, discord_id: int, guild_id) -> dict:
        """Admin: reset a player's free dig cooldown."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("That player doesn't have a tunnel.")
        self.dig_repo.update_tunnel(discord_id, guild_id, last_dig_at=0)
        return self._ok(reset=True)

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
            return self._ok(current_tier="Wooden", current_tier_index=0, next_tier=None, eligible=False)

        tunnel = dict(tunnel)
        current_idx = tunnel.get("pickaxe_tier", 0)
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

        # Phase 2 penalty for P4+ bosses
        phase2_penalty = 0.0
        if boss_progress.get(str(at_boss)) == "phase1_defeated" and at_boss in BOSS_PHASE2:
            phase2_penalty = abs(BOSS_PHASE2[at_boss].win_odds_penalty)

        win_chance = base_odds - depth_penalty - prestige_penalty - phase2_penalty + cheer_bonus
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

        # Apply ascension boss payout modifier (P4+)
        ascension = self._get_ascension_effects(prestige_level)
        boss_payout_mult = 1.0 + ascension.get("boss_payout_multiplier", 0)

        if won:
            # Check if boss has secret phase 2 (P4+ ascension)
            current_status = boss_progress.get(str(at_boss), "active")
            needs_phase2 = (ascension.get("boss_phase2", False)
                            and at_boss in BOSS_PHASE2
                            and current_status == "active")

            if needs_phase2:
                # Phase 1 victory — boss transforms, fight again
                boss_progress[str(at_boss)] = "phase1_defeated"
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    boss_progress=json.dumps(boss_progress),
                    boss_attempts=attempts,
                )

                phase2 = BOSS_PHASE2[at_boss]
                p2_dialogue = phase2.dialogue[min(attempts - 1, len(phase2.dialogue) - 1)]

                self.dig_repo.log_action(
                    discord_id=discord_id, guild_id=guild_id,
                    action_type="boss_fight",
                    details=json.dumps({
                        "boundary": at_boss, "won": True, "risk": risk_tier,
                        "phase": 1, "wager": wager,
                    }),
                )

                return self._ok(
                    won=True,
                    phase=1,
                    phase2_incoming=True,
                    boss_name=boss_name,
                    phase2_name=phase2.name,
                    phase2_title=phase2.title,
                    boundary=at_boss,
                    risk_tier=risk_tier,
                    win_chance=round(win_chance, 2),
                    jc_delta=0,
                    payout=0,
                    new_depth=depth,
                    dialogue=p2_dialogue,
                    achievements=[],
                )

            # Full victory (or phase 2 already cleared)
            new_depth = at_boss
            base_jc = int(wager * multiplier) if wager > 0 else random.randint(5, 15)
            jc_delta = int(base_jc * boss_payout_mult)

            boss_progress[str(at_boss)] = "defeated"
            stat_point_awarded = self._award_boss_stat_point_if_first(
                discord_id, guild_id, tunnel, at_boss
            )
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                boss_progress=json.dumps(boss_progress),
                boss_attempts=0,
                cheer_data=None,  # Clear cheers
            )

            if wager > 0:
                self.player_repo.add_balance(discord_id, guild_id, int(wager * (multiplier * boss_payout_mult - 1)))
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
                    "stat_point_awarded": stat_point_awarded,
                }),
            )

            return self._ok(
                won=True,
                phase=2 if current_status == "phase1_defeated" else None,
                boss_name=boss_name,
                boundary=at_boss,
                risk_tier=risk_tier,
                win_chance=round(win_chance, 2),
                jc_delta=jc_delta,
                payout=jc_delta,
                new_depth=new_depth,
                dialogue=defeat_msg,
                achievements=achievements,
                stat_point_awarded=stat_point_awarded,
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

        run_score = self._calculate_run_score(tunnel) if can else 0

        # Prepare mutation choices if P8+
        mutation_info = None
        if can and (prestige_level + 1) >= 8:
            forced, choices = self._roll_mutations_for_prestige()
            mutation_info = {"forced": forced, "choices": choices}

        return self._ok(
            can_prestige=can,
            reason=reason,
            prestige_level=prestige_level,
            available_perks=[p for p in PRESTIGE_PERKS if p not in self._get_prestige_perks(tunnel)],
            run_score=run_score,
            mutation_info=mutation_info,
        )

    def prestige(self, discord_id: int, guild_id, perk_choice: str,
                  mutation_choice: str | None = None) -> dict:
        """
        Prestige: reset tunnel, keep pickaxe, gain a perk.

        perk_choice: ID of the perk to select.
        mutation_choice: ID of chosen mutation (P8+ only, None if < P8).
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

        # Calculate run score before reset
        run_score = self._calculate_run_score(tunnel)
        best_score = max(tunnel.get("best_run_score", 0) or 0, run_score)
        total_score = (tunnel.get("total_prestige_score", 0) or 0) + run_score

        # Roll mutations for P8+
        mutations_json = None
        mutation_info = None
        if prestige_level >= 8:
            forced, choices = self._roll_mutations_for_prestige()
            active_mutations = [forced]
            if mutation_choice and MUTATION_BY_ID.get(mutation_choice):
                chosen = {"id": mutation_choice,
                          "name": MUTATION_BY_ID[mutation_choice].name,
                          "description": MUTATION_BY_ID[mutation_choice].description,
                          "positive": MUTATION_BY_ID[mutation_choice].positive}
                active_mutations.append(chosen)
            elif choices:
                active_mutations.append(choices[0])
            mutations_json = json.dumps(active_mutations)
            mutation_info = {"forced": forced, "chosen": active_mutations[-1] if len(active_mutations) > 1 else None}

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
            best_run_score=best_score,
            current_run_jc=0,
            current_run_artifacts=0,
            current_run_events=0,
            total_prestige_score=total_score,
            mutations=mutations_json,
        )

        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="prestige",
            details=json.dumps({
                "level": prestige_level, "perk": perk_choice,
                "run_score": run_score, "mutations": mutation_info,
            }),
        )

        # Ascension modifiers active at new level
        ascension = ASCENSION_MODIFIERS.get(prestige_level)
        ascension_info = None
        if ascension:
            ascension_info = {"name": ascension.name,
                              "penalty": ascension.penalty,
                              "reward": ascension.reward,
                              "gameplay": ascension.gameplay}

        return self._ok(
            prestige_level=prestige_level,
            perk_chosen=perk_choice,
            perks=current_perks,
            run_score=run_score,
            best_run_score=best_score,
            total_prestige_score=total_score,
            ascension_unlocked=ascension_info,
            mutations=mutation_info,
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

    def roll_artifact(self, discord_id: int, guild_id, depth: int, *, extra_rate_mod: float = 1.0) -> dict | None:
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

    def roll_event(self, depth: int, luminosity: int = 100,
                   prestige_level: int = 0) -> dict | None:
        """
        Roll for a random event with layer-specific rates, rarity, and prestige gating.

        Returns event info dict, or None if no event triggers.
        """
        layer = self._get_layer(depth)
        layer_name = layer.get("name", "Dirt")
        is_pitch_black = luminosity <= 0
        ascension = self._get_ascension_effects(prestige_level)

        # Filter eligible events by depth, layer, darkness, and prestige
        eligible = [
            e for e in EVENT_POOL
            if depth >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth <= e["max_depth"])
            and (e.get("layer") is None or e["layer"] == layer_name)
            and (not e.get("requires_dark") or is_pitch_black)
            and prestige_level >= e.get("min_prestige", 0)
        ]

        # Non-darkness events are excluded at pitch black if darkness events exist
        if is_pitch_black:
            dark_events = [e for e in eligible if e.get("requires_dark")]
            if dark_events:
                eligible = dark_events + [e for e in eligible if not e.get("requires_dark")]

        if not eligible:
            return None

        # Rarity-weighted selection with ascension modifiers
        rare_mult = 1.0 + ascension.get("rare_event_multiplier", 0)
        legendary_mult = 1.0 + ascension.get("legendary_event_multiplier", 0)
        adjusted_weights = dict(RARITY_WEIGHTS)
        adjusted_weights["rare"] = int(RARITY_WEIGHTS["rare"] * rare_mult)
        adjusted_weights["legendary"] = int(RARITY_WEIGHTS["legendary"] * legendary_mult)

        weighted = [(e, adjusted_weights.get(e.get("rarity", "common"), 70)) for e in eligible]
        events, w = zip(*weighted)
        event = random.choices(events, weights=w, k=1)[0]

        return {
            "id": event["id"],
            "name": event["name"],
            "description": event["description"],
            "complexity": event.get("complexity", "choice"),
            "safe_option": event.get("safe_option"),
            "risky_option": event.get("risky_option"),
            "desperate_option": event.get("desperate_option"),
            "boon_options": event.get("boon_options"),
            "buff_on_success": event.get("buff_on_success"),
            "rarity": event.get("rarity", "common"),
        }

    def resolve_event(self, discord_id: int, guild_id, event_id: str, choice: str,
                      chained: bool = False) -> dict:
        """Apply event outcome based on safe/risky/desperate/boon choice."""
        event = next((e for e in EVENT_POOL if e["id"] == event_id), None)
        if event is None:
            return self._error("Unknown event.")

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        depth = tunnel.get("depth", 0)
        luminosity = tunnel.get("luminosity", LUMINOSITY_MAX)
        prestige_level = tunnel.get("prestige_level", 0) or 0
        ascension = self._get_ascension_effects(prestige_level)

        # Pitch black: force risky (safe option removed)
        if LUMINOSITY_PITCH_FORCE_RISKY and luminosity <= LUMINOSITY_PITCH_BLACK and choice == "safe" and event.get("risky_option"):
            choice = "risky"

        # Handle boon choice — apply selected buff
        if choice.startswith("boon_") and event.get("boon_options"):
            boon_idx = int(choice.split("_")[1]) if choice.split("_")[1].isdigit() else 0
            boons = event["boon_options"]
            if boon_idx >= len(boons):
                return self._error("Invalid boon selection.")
            boon = boons[boon_idx]
            # Apply buff
            self.set_temp_buff(discord_id, guild_id, boon)
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="event",
                details=json.dumps({"event_id": event_id, "choice": choice, "boon": boon.get("name", boon.get("id"))}),
            )
            return self._ok(
                event_name=event.get("name", "Unknown Event"),
                choice=choice,
                jc_delta=0,
                depth_delta=0,
                message=f"You chose {boon.get('name', 'a boon')}!",
                buff_applied=boon,
            )

        # Map choice to option data
        option = None
        if choice == "safe":
            option = event.get("safe_option")
        elif choice == "risky":
            option = event.get("risky_option")
        elif choice == "desperate":
            option = event.get("desperate_option")

        if option is None:
            # Fall back to legacy outcomes format
            outcomes = event.get("outcomes", {})
            outcome = outcomes.get(choice)
            if outcome is None:
                return self._error(f"Invalid choice: {choice}")

            jc_delta = 0
            depth_delta = 0
            message = outcome.get("message", "Nothing happened.")
            if "jc" in outcome:
                jc_range = outcome["jc"]
                jc_delta = random.randint(jc_range[0], jc_range[1]) if isinstance(jc_range, list) else jc_range
                self.player_repo.add_balance(discord_id, guild_id, jc_delta)
            if "depth" in outcome:
                depth_range = outcome["depth"]
                depth_delta = random.randint(depth_range[0], depth_range[1]) if isinstance(depth_range, list) else depth_range
                new_depth = max(0, depth + depth_delta)
                self.dig_repo.update_tunnel(discord_id, guild_id, depth=new_depth)

            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="event",
                details=json.dumps({"event_id": event_id, "choice": choice, "jc_delta": jc_delta, "depth_delta": depth_delta}),
            )
            return self._ok(event_name=event.get("name", "Unknown Event"), choice=choice,
                            jc_delta=jc_delta, depth_delta=depth_delta, message=message)

        # New-style EventChoice resolution
        success_chance = option.get("success_chance", 1.0)

        # Dark luminosity: risky/desperate options are harder
        if choice in ("risky", "desperate") and luminosity < LUMINOSITY_DIM:
            success_chance = max(0.05, success_chance - LUMINOSITY_DARK_RISKY_PENALTY)

        # P9 Cruel Echoes: safe options now have 10% failure chance
        cruel_fail = ascension.get("cruel_safe_fail", 0)
        if choice == "safe" and cruel_fail > 0 and option.get("failure") is not None:
            success_chance = min(success_chance, 1.0 - cruel_fail)
        elif choice == "safe" and cruel_fail > 0 and option.get("failure") is None and random.random() < cruel_fail:
            # Safe options with no failure defined — cruel echoes creates one
            new_depth = max(0, depth - 1)
            self.dig_repo.update_tunnel(discord_id, guild_id, depth=new_depth)
            self.player_repo.add_balance(discord_id, guild_id, -1)
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="event",
                details=json.dumps({"event_id": event_id, "choice": choice, "cruel_echoes": True}),
            )
            return self._ok(
                event_name=event.get("name", "Unknown Event"), choice=choice,
                jc_delta=-1, depth_delta=-1, message="Cruel Echoes! Even safety betrays you. Lost 1 block and 1 JC.",
                cruel_echoes=True,
            )

        succeeded = random.random() < success_chance
        result = option.get("success") if succeeded else option.get("failure")

        if result is None:
            result = option.get("success")  # fallback if no failure defined

        advance = result.get("advance", 0)
        jc = result.get("jc", 0)
        cave_in = result.get("cave_in", False)
        description = result.get("description", "Something happened.")

        # P7 chain JC multiplier: chained events get 1.5x JC
        if chained and jc > 0:
            chain_mult = ascension.get("chain_jc_multiplier", 1.0)
            if chain_mult > 1.0:
                jc = int(jc * chain_mult)

        # Apply depth change
        new_depth = max(0, depth + advance)
        if advance != 0:
            self.dig_repo.update_tunnel(discord_id, guild_id, depth=new_depth)

        # Apply JC change
        if jc != 0:
            self.player_repo.add_balance(discord_id, guild_id, jc)

        # Apply buff if risky/desperate success
        buff_applied = None
        if succeeded and choice in ("risky", "desperate") and event.get("buff_on_success"):
            buff_data = event["buff_on_success"]
            self.set_temp_buff(discord_id, guild_id, buff_data)
            buff_applied = buff_data


        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="event",
            details=json.dumps({
                "event_id": event_id, "choice": choice, "succeeded": succeeded,
                "advance": advance, "jc": jc, "cave_in": cave_in,
            }),
        )

        # Check for event chaining (P7+)
        chain_event = self._chain_event(new_depth, prestige_level,
                                         event.get("rarity", "common"),
                                         tunnel.get("luminosity", 100))

        return self._ok(
            event_name=event.get("name", "Unknown Event"),
            choice=choice,
            succeeded=succeeded,
            jc_delta=jc,
            depth_delta=advance,
            cave_in=cave_in,
            message=description,
            buff_applied=buff_applied,
            chain_event=chain_event,
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
