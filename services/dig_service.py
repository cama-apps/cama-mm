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
import unicodedata

from domain.models.dig_gear import GearLoadout, GearPiece, GearSlot
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_constants import (
    ACHIEVEMENTS,
    ARTIFACT_POOL,
    ASCENSION_MODIFIERS,
    BOSS_ARCHETYPE_BY_ID,
    BOSS_ARCHETYPES,
    BOSS_ASCII,
    BOSS_BOUNDARIES,
    BOSS_DIALOGUE,
    BOSS_DIALOGUE_V2,
    BOSS_DUEL_STATS,
    BOSS_FREE_FIGHT_ACCURACY_MOD,
    BOSS_HP_REGEN_PER_2_HOURS,
    BOSS_NAMES,
    BOSS_PAYOUTS,
    BOSS_PHASE2,
    BOSS_PHASE3,
    BOSS_PHASES,
    BOSS_PRESTIGE_BONUS,
    BOSS_ROUND_CAP,
    BOSS_TIER_BONUS,
    CAVE_IN_BLOCK_LOSS_MAX,
    CAVE_IN_BLOCK_LOSS_MIN,
    CHEER_COOLDOWN_SECONDS,
    CONSUMABLE_ITEMS,
    CORRUPTION_BAD,
    CORRUPTION_WEIRD,
    DIG_TIPS,
    EVENT_CHAIN_CHANCE,
    EVENT_POOL,
    FREE_DIG_COOLDOWN,
    GEAR_BOSS_DROP_RATE,
    GEAR_DROP_DEPTH_TIER_MAP,
    GEAR_MAX_DURABILITY,
    GEAR_REPAIR_COST_PCT,
    GEAR_TIER_TABLES,
    INJURY_SLOW_COOLDOWN,
    ITEM_PRICES,
    LAYER_WEATHER_POOL,
    LAYERS,
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK,
    LUMINOSITY_DARK_CAVE_IN_BONUS,
    LUMINOSITY_DARK_EVENT_MULTIPLIER,
    LUMINOSITY_DARK_HIT_PENALTY,
    LUMINOSITY_DARK_JC_MULTIPLIER,
    LUMINOSITY_DARK_RISKY_PENALTY,
    LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP,
    LUMINOSITY_DEEP_DRAIN_START_DEPTH,
    LUMINOSITY_DIM,
    LUMINOSITY_DIM_CAVE_IN_BONUS,
    LUMINOSITY_DIM_EVENT_MULTIPLIER,
    LUMINOSITY_DIM_HIT_PENALTY,
    LUMINOSITY_DRAIN_PER_DIG,
    LUMINOSITY_MAX,
    LUMINOSITY_PITCH_BLACK,
    LUMINOSITY_PITCH_BOSS_DMG_BONUS,
    LUMINOSITY_PITCH_CAVE_IN_BONUS,
    LUMINOSITY_PITCH_EVENT_MULTIPLIER,
    LUMINOSITY_PITCH_FORCE_RISKY,
    LUMINOSITY_PITCH_HIT_PENALTY,
    LUMINOSITY_PITCH_JC_MULTIPLIER,
    LUMINOSITY_REFILL_PER_DAY,
    MAX_INVENTORY_SIZE,
    MAX_PRESTIGE,
    MILESTONES,
    MUTATION_BY_ID,
    MUTATIONS_POOL,
    PAID_DIG_COSTS,
    PHASE_TRANSITION_EVENTS,
    PICKAXE_TIERS,
    PINNACLE_BASE_JC_REWARD,
    PINNACLE_BOSSES,
    PINNACLE_DEPTH,
    PINNACLE_FORESHADOW_LINES,
    PINNACLE_JC_PER_PRESTIGE,
    PINNACLE_POOL_IDS,
    PINNACLE_RELIC_BASE_NAME,
    PINNACLE_RELIC_STAT_POOL,
    PINNACLE_RELIC_SUFFIX_POOL,
    PINNACLE_REPROC_DEPTH,
    PLAYER_HIT_CEILING,
    PLAYER_HIT_FLOOR,
    PRESTIGE_HARD_CAP,
    PRESTIGE_PERKS,
    RELIC_SLOTS_BASE,
    RETREAT_BLOCK_LOSS_MAX,
    RETREAT_BLOCK_LOSS_MIN,
    RETREAT_COOLDOWN_SECONDS,
    STREAKS,
    TUNNEL_NAME_ADJECTIVES,
    TUNNEL_NAME_NOUNS,
    TUNNEL_NAME_SILLY,
    TUNNEL_NAME_TITLES,
    WEATHER_BY_ID,
    WIN_CHANCE_CAP,
    WIN_CHANCE_FLOOR,
)

logger = logging.getLogger("cama_bot.services.dig")

RARITY_WEIGHTS = {"common": 70, "uncommon": 20, "rare": 12, "legendary": 6}
DIG_STARTING_STAT_POINTS = 5
DIG_BOSS_STAT_POINT_BONUS = 1
MINER_BACKSTORY_MAX_LENGTH = 600
STRENGTH_MAX_ADVANCE_INTERVAL = 2
STRENGTH_MIN_ADVANCE_INTERVAL = 5
SMARTS_CAVE_IN_REDUCTION = 0.02
STAMINA_COOLDOWN_REDUCTION = 0.04
STAMINA_MAX_REDUCTION = 0.50

def _splash_trigger_matches(trigger: str, succeeded: bool) -> bool:
    """Does the event's splash config fire on this outcome?"""
    if trigger == "always":
        return True
    if trigger == "success":
        return bool(succeeded)
    # default "failure"
    return not succeeded


def _splash_to_dict(result) -> dict | None:
    """Serialize a :class:`SplashResult` for return from resolve_event."""
    if result is None or not getattr(result, "victims", None):
        return None
    return {
        "strategy": result.strategy,
        "event_name": result.event_name,
        "victims": [{"discord_id": vid, "amount": amt} for vid, amt in result.victims],
        "total_burned": result.total_burned,
        "mode": getattr(result, "mode", "burn"),
    }


def _approx_duel_win_prob(
    *, player_hp: int, boss_hp: int,
    player_hit: float, player_dmg: int,
    boss_hit: float, boss_dmg: int,
    trials: int = 500,
) -> float:
    """Estimate the probability the player wins a boss HP duel.

    Used by ``scout_boss`` to surface an approximate win% to players
    without resolving an actual fight. Monte Carlo with a local ``Random``
    so the estimate does not consume the global RNG stream (important for
    deterministic dig tests).
    """
    if player_hp <= 0 or boss_hp <= 0:
        return 0.0
    if trials <= 0:
        return 0.0
    rng = random.Random()
    wins = 0
    for _ in range(trials):
        php, bhp = player_hp, boss_hp
        while True:
            if rng.random() < player_hit:
                bhp -= player_dmg
            if bhp <= 0:
                wins += 1
                break
            if rng.random() < boss_hit:
                php -= boss_dmg
            if php <= 0:
                break
    raw = wins / trials
    return max(WIN_CHANCE_FLOOR, min(WIN_CHANCE_CAP, raw))


def _luminosity_combat_penalty(luminosity: int) -> tuple[float, int]:
    """Translate current luminosity into (player_hit_offset, boss_dmg_bonus).

    Bright (>=76) → (0, 0)
    Dim (26-75) → (-0.03, 0)
    Dark (1-25) → (-0.08, 0)
    Pitch black (0) → (-0.15, +1)
    """
    if luminosity >= LUMINOSITY_BRIGHT:
        return (0.0, 0)
    if luminosity >= LUMINOSITY_DIM:
        return (-LUMINOSITY_DIM_HIT_PENALTY, 0)
    if luminosity >= LUMINOSITY_DARK:
        return (-LUMINOSITY_DARK_HIT_PENALTY, 0)
    return (-LUMINOSITY_PITCH_HIT_PENALTY, LUMINOSITY_PITCH_BOSS_DMG_BONUS)


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
        """Bundle a player's three equipped gear slots + their relics."""
        equipped = self.dig_repo.get_equipped_gear(discord_id, guild_id)
        return GearLoadout(
            weapon=self._hydrate_gear_piece(equipped.get("weapon")),
            armor=self._hydrate_gear_piece(equipped.get("armor")),
            boots=self._hydrate_gear_piece(equipped.get("boots")),
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
        return out

    def _resolve_persisted_boss_hp(
        self,
        boss_progress: dict,
        at_boss: int | str,
        fresh_hp: int,
        now: int,
    ) -> tuple[int, int]:
        """Apply persisted-HP carry-over and time-based regen to a boss fight.

        Returns ``(starting_hp, hp_max)``. ``hp_max`` is always ``fresh_hp``
        (the freshly-computed scaled boss HP for this fight) so the boss can
        regen back to it. ``starting_hp`` is:
          - ``hp_remaining`` from the last unfinished engagement, plus regen
            of ``BOSS_HP_REGEN_PER_2_HOURS`` per two-hour block since
            ``last_engaged_at``, capped at ``hp_max``;
          - ``fresh_hp`` if no persisted HP exists.

        ``at_boss`` is normally an int boundary depth (e.g. 25), but pinnacle
        callers pass a composite phase key string like ``"300:1"``.
        """
        entry = boss_progress.get(str(at_boss))
        if not isinstance(entry, dict):
            return fresh_hp, fresh_hp
        hp_remaining = entry.get("hp_remaining")
        hp_max = entry.get("hp_max", fresh_hp)
        if hp_remaining is None or hp_max is None:
            return fresh_hp, fresh_hp
        try:
            hp_remaining = int(hp_remaining)
            hp_max = int(hp_max)
        except (TypeError, ValueError):
            return fresh_hp, fresh_hp
        last_engaged = entry.get("last_engaged_at")
        if last_engaged is not None:
            try:
                two_hour_blocks = max(0, (now - int(last_engaged)) // 7200)
            except (TypeError, ValueError):
                two_hour_blocks = 0
            hp_remaining = min(hp_max, hp_remaining + two_hour_blocks * BOSS_HP_REGEN_PER_2_HOURS)
        return max(1, hp_remaining), hp_max

    def _persist_boss_hp_after_fight(
        self,
        boss_progress: dict,
        at_boss: int | str,
        boss_id: str,
        ending_hp: int,
        hp_max: int,
        won: bool,
        outcome: str,
        now: int,
    ) -> None:
        """Update boss_progress entry with post-fight HP and outcome.

        Caller writes ``boss_progress`` back to the database afterwards. The
        function only mutates the dict in place. ``ending_hp`` is the boss
        HP at the moment the fight ended (0 on a player win, otherwise the
        leftover after the duel loop). ``at_boss`` is normally an int boundary
        depth (e.g. 25), but pinnacle callers pass a composite phase key
        string like ``"300:1"``.
        """
        raw = boss_progress.get(str(at_boss))
        if isinstance(raw, dict):
            entry = dict(raw)
        elif isinstance(raw, str):
            entry = {"status": raw}
        else:
            entry = {}
        entry["hp_remaining"] = max(0, int(ending_hp))
        entry["hp_max"] = int(hp_max)
        entry["last_engaged_at"] = int(now)
        entry["last_outcome"] = outcome
        entry["first_meet_seen"] = True
        if boss_id and not entry.get("boss_id"):
            entry["boss_id"] = boss_id
        if not won:
            entry.setdefault("status", "active")
        boss_progress[str(at_boss)] = entry

    def _scale_boss_stats(
        self,
        stats: dict,
        *,
        boss_id: str,
        at_boss: int,
        prestige_level: int,
        echo_applied: bool = False,
        archetype_name: str | None = None,
    ) -> dict:
        """Apply archetype + depth + prestige + echo to boss-side stats.

        Returns ``(boss_hp, boss_hit, boss_dmg)`` keys updated. Player-side
        stats are passed through; the caller still applies depth/prestige
        hit penalties, cheers, phase2/3 penalties, lum penalty, and clamping
        to player_hit. Order: archetype first, then linear depth/prestige
        scaling, then echo HP discount.

        ``archetype_name`` overrides the per-boss archetype lookup — used
        by the pinnacle resolver to apply a different archetype per phase
        (e.g. Forgotten King: Tank → Glass Cannon → Slippery).
        """
        if archetype_name is None:
            archetype_name = BOSS_ARCHETYPE_BY_ID.get(boss_id, "bruiser")
        archetype = BOSS_ARCHETYPES.get(archetype_name, BOSS_ARCHETYPES["bruiser"])

        # Boundary key for the tier lookup. Pinnacle uses depth 300; for
        # off-boundary calls (defensive), pick the highest boundary <= at_boss.
        tier_key = at_boss if at_boss in BOSS_TIER_BONUS else max(
            (k for k in BOSS_TIER_BONUS if k <= at_boss), default=25,
        )
        tier = BOSS_TIER_BONUS[tier_key]
        prestige = BOSS_PRESTIGE_BONUS.get(prestige_level, BOSS_PRESTIGE_BONUS[max(BOSS_PRESTIGE_BONUS)])

        # Boss HP: archetype mult, then tier+prestige adds from tables, then echo.
        boss_hp = float(stats["boss_hp"]) * archetype["hp_mult"]
        boss_hp += tier["hp"] + prestige["hp"]
        boss_hp = max(1, int(round(boss_hp)))
        if echo_applied:
            boss_hp = max(1, int(round(boss_hp * 0.75)))

        # Boss hit: archetype offset + tier + prestige, clamped.
        boss_hit = float(stats["boss_hit"]) + archetype["hit_offset"]
        boss_hit += tier["hit"] + prestige["hit"]
        boss_hit = max(0.05, min(0.95, boss_hit))

        # Boss dmg: archetype offset + tier + prestige, floored at 1.
        boss_dmg = int(stats["boss_dmg"]) + int(archetype["dmg_offset"])
        boss_dmg += int(tier["dmg"]) + int(prestige["dmg"])
        boss_dmg = max(1, boss_dmg)

        out = dict(stats)
        out["boss_hp"] = boss_hp
        out["boss_hit"] = boss_hit
        out["boss_dmg"] = boss_dmg
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
        return {
            "weapon": serialize(loadout.weapon),
            "armor":  serialize(loadout.armor),
            "boots":  serialize(loadout.boots),
            "relics": list(loadout.relics),
        }

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
        damaged = [r for r in rows if int(r["durability"]) < GEAR_MAX_DURABILITY]
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

    def equip_relic_for_player(self, discord_id: int, guild_id, artifact_db_id: int) -> dict:
        """Equip a relic, enforcing the prestige-scaled cap.

        The cap is ``prestige_level + RELIC_SLOTS_BASE``. Equipping over the
        cap is rejected — caller (the panel) is expected to ask the user to
        unequip something first.
        """
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        target = next((a for a in artifacts if int(a["id"]) == int(artifact_db_id)), None)
        if target is None:
            return self._error("That relic isn't in your inventory.")
        if int(target.get("is_relic", 0)) != 1:
            return self._error("That artifact isn't a relic and can't be equipped.")
        if int(target.get("equipped", 0)) == 1:
            return self._error("That relic is already equipped.")
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        prestige = int(tunnel.get("prestige_level", 0) or 0) if tunnel else 0
        cap = prestige + RELIC_SLOTS_BASE
        equipped_count = self.dig_repo.count_equipped_relics(discord_id, guild_id)
        if equipped_count >= cap:
            return self._error(
                f"You've hit your relic cap ({cap}). Unequip one first.",
            )
        self.dig_repo.equip_relic(int(artifact_db_id), True)
        return self._ok(artifact_id=target.get("artifact_id"), cap=cap)

    def unequip_relic_for_player(self, discord_id: int, guild_id, artifact_db_id: int) -> dict:
        """Unequip a relic owned by this player."""
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        target = next((a for a in artifacts if int(a["id"]) == int(artifact_db_id)), None)
        if target is None:
            return self._error("That relic isn't in your inventory.")
        self.dig_repo.unequip_relic(int(artifact_db_id))
        return self._ok(artifact_id=target.get("artifact_id"))

    def _maybe_drop_gear(self, discord_id: int, guild_id, at_boss: int) -> dict | None:
        """Roll a single boss-drop after a kill. Returns the drop payload or None."""
        if at_boss not in GEAR_DROP_DEPTH_TIER_MAP:
            return None
        if random.random() >= GEAR_BOSS_DROP_RATE:
            return None
        tier = GEAR_DROP_DEPTH_TIER_MAP[at_boss]
        slot_choice = random.choice(["weapon", "armor", "boots"])
        gear_id = self.dig_repo.add_gear(
            discord_id, guild_id, slot_choice, tier, source="boss_drop",
        )
        try:
            slot_enum = GearSlot(slot_choice)
            name = GEAR_TIER_TABLES[slot_enum][tier].name
        except (ValueError, KeyError, IndexError):
            name = f"{slot_choice} (tier {tier})"
        return {"gear_id": gear_id, "slot": slot_choice, "tier": tier, "name": name}

    def _get_queued_items_for_tunnel(self, discord_id: int, guild_id) -> list[dict]:
        """Get items queued for next dig from inventory table."""
        items = self.dig_repo.get_queued_items(discord_id, guild_id)
        return [{"type": i.get("item_type"), "id": i.get("id")} for i in items]

    def _get_boss_progress(self, tunnel: dict) -> dict:
        """Get boss defeat state as a flat ``{depth_str: status_str}`` dict.

        Normalizes BOTH the legacy string-status shape
        (``{"25": "active"}``) and the new ``{"boss_id", "status"}`` shape
        (``{"25": {"boss_id": "grothak", "status": "active"}}``) down to a
        plain status-only dict, so existing callers that branch on status
        keep working regardless of which format the JSON is in.

        Missing keys default to "active" (prevents prestige with only old
        bosses).
        """
        canonical = {str(b): "active" for b in BOSS_BOUNDARIES}
        raw = tunnel.get("boss_progress")
        if not raw:
            return canonical
        try:
            stored = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return canonical
        normalized: dict = {}
        for key, val in stored.items():
            if isinstance(val, dict):
                normalized[key] = val.get("status", "active")
            else:
                normalized[key] = val
        canonical.update(normalized)
        return canonical

    def _get_locked_boss_id(self, tunnel: dict, depth: int) -> str:
        """Return the locked boss_id for this tunnel at this depth.

        Reads the ``boss_progress`` JSON for a ``{"boss_id", "status"}``
        entry. Falls back to the grandfathered boss (first entry in
        ``BOSSES_BY_TIER[depth]``) if not yet locked, matching the pre-feature
        behaviour so display paths don't break during partial rollouts.
        """
        raw = tunnel.get("boss_progress")
        if raw:
            try:
                stored = json.loads(raw)
            except (json.JSONDecodeError, TypeError):
                stored = {}
            entry = stored.get(str(depth))
            if isinstance(entry, dict):
                bid = entry.get("boss_id")
                if bid:
                    return bid
        from services.dig_constants import get_boss_pool_for_tier as _pool
        pool = _pool(depth)
        return pool[0].boss_id if pool else ""

    def _ensure_boss_locked(
        self, discord_id: int, guild_id, tunnel: dict, depth: int,
    ):
        """Roll + persist the tunnel's boss at this tier, or return existing.

        Called from the mid-fight state machine entry points. Safe to call
        repeatedly: once a boss is locked the same BossDef is returned. The
        locked boss_id is written into ``tunnels.boss_progress`` under the
        depth key, alongside the current status.
        """
        from services.dig_constants import (
            BOSSES_BY_ID as _BOSSES_BY_ID,
        )
        from services.dig_constants import (
            get_boss_pool_for_tier as _pool,
        )
        raw = tunnel.get("boss_progress")
        try:
            progress = json.loads(raw) if raw else {}
        except (json.JSONDecodeError, TypeError):
            progress = {}
        entry = progress.get(str(depth))
        if isinstance(entry, dict):
            bid = entry.get("boss_id")
            if bid and bid in _BOSSES_BY_ID:
                return _BOSSES_BY_ID[bid]

        pool = _pool(depth)
        if not pool:
            raise ValueError(f"No boss pool for tier {depth}")
        boss = random.Random().choice(pool)
        status = (
            entry.get("status", "active")
            if isinstance(entry, dict)
            else (entry if isinstance(entry, str) else "active")
        )
        progress[str(depth)] = {"boss_id": boss.boss_id, "status": status}
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            boss_progress=json.dumps(progress),
        )
        tunnel["boss_progress"] = json.dumps(progress)
        return boss

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
        """Return the boss boundary if depth is exactly at one and boss is in
        an unfinished state (active / phase1_defeated / phase2_defeated).

        The pinnacle (depth 300) is gated: it only fires once all 7 prior
        tier bosses are marked defeated.
        """
        for b in BOSS_BOUNDARIES:
            entry = boss_progress.get(str(b))
            status = entry.get("status") if isinstance(entry, dict) else entry
            if depth == b - 1 and status in ("active", "phase1_defeated", "phase2_defeated"):
                return b
        # Pinnacle: triggers after all 7 tiers cleared, when depth hits
        # PINNACLE_DEPTH-1. Also re-procs if the player has tunneled past
        # PINNACLE_REPROC_DEPTH without defeating it (catch-up for legacy
        # tunnels that pre-date the pinnacle).
        at_pinnacle_threshold = depth == PINNACLE_DEPTH - 1
        in_reproc_window = depth >= PINNACLE_REPROC_DEPTH
        if at_pinnacle_threshold or in_reproc_window:
            all_tiers_cleared = all(
                (
                    (e.get("status") if isinstance(e, dict) else e) == "defeated"
                )
                for b in BOSS_BOUNDARIES
                for e in (boss_progress.get(str(b)),)
                if e is not None
            ) and len([
                b for b in BOSS_BOUNDARIES if boss_progress.get(str(b)) is not None
            ]) == len(BOSS_BOUNDARIES)
            pinnacle_entry = boss_progress.get(str(PINNACLE_DEPTH))
            pinnacle_status = (
                pinnacle_entry.get("status") if isinstance(pinnacle_entry, dict)
                else pinnacle_entry
            )
            if all_tiers_cleared and pinnacle_status in (
                None, "active", "phase1_defeated", "phase2_defeated",
            ):
                return PINNACLE_DEPTH
        return None

    def _is_pinnacle_depth(self, depth: int) -> bool:
        """True if the given depth is the pinnacle boundary."""
        return depth == PINNACLE_DEPTH

    def _ensure_pinnacle_locked(
        self, discord_id: int, guild_id, tunnel: dict,
    ) -> str:
        """Roll + persist the tunnel's pinnacle from PINNACLE_POOL_IDS.

        Returns the locked ``pinnacle_boss_id`` (Slay-the-Spire-style
        rotating pool). Idempotent: once locked, subsequent calls return
        the same id. Stored on ``tunnels.pinnacle_boss_id``.
        """
        existing = tunnel.get("pinnacle_boss_id")
        if existing and existing in PINNACLE_BOSSES:
            return existing
        choice = random.Random().choice(PINNACLE_POOL_IDS)
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            pinnacle_boss_id=choice,
            pinnacle_phase=1,  # start at phase 1
        )
        tunnel["pinnacle_boss_id"] = choice
        tunnel["pinnacle_phase"] = 1
        return choice

    def _render_boss_bark(self, template: str, tunnel: dict) -> str:
        """Render a dialogue line by substituting stat-aware tokens.

        Supported tokens:
          {streak}          → streak_days (defaults to 1)
          {depth}           → current depth
          {prestige}        → current prestige_level
          {killed_boss_name} → name of a previously defeated boss in this delve;
                                falls back to "the early dark" when none.
        Lines without tokens render verbatim.
        """
        try:
            killed_name = "the early dark"
            try:
                bp = json.loads(tunnel.get("boss_progress") or "{}")
                defeated_ids = []
                for _depth, entry in bp.items():
                    status = entry.get("status") if isinstance(entry, dict) else entry
                    boss_id = entry.get("boss_id", "") if isinstance(entry, dict) else ""
                    if status == "defeated" and boss_id:
                        defeated_ids.append(boss_id)
                if defeated_ids:
                    from services.dig_constants import (
                        get_boss_by_id as _get_boss_by_id,
                    )
                    boss_def = _get_boss_by_id(random.choice(defeated_ids))
                    if boss_def is not None:
                        killed_name = boss_def.name
            except (json.JSONDecodeError, TypeError):
                pass

            return template.format(
                streak=tunnel.get("streak_days", 1) or 1,
                depth=tunnel.get("depth", 0) or 0,
                prestige=tunnel.get("prestige_level", 0) or 0,
                killed_boss_name=killed_name,
            )
        except (KeyError, IndexError):
            return template

    def _pick_boss_dialogue_line(
        self, boss_id: str, slot: str, fallback: str,
    ) -> str:
        """Random-pick a hand-authored line from BOSS_DIALOGUE_V2[boss_id][slot].

        Falls back to ``fallback`` when the boss or slot is missing — this
        keeps grandfathered bosses without a v2 entry from breaking the embed.
        """
        boss_lines = BOSS_DIALOGUE_V2.get(boss_id, {}).get(slot, [])
        if not boss_lines:
            return fallback
        return random.choice(boss_lines)

    def _read_boss_progress_entry(self, boss_progress: dict, boundary: int) -> dict:
        """Normalize a boss_progress entry to dict shape with default fields.

        Legacy string values (``"active"`` / ``"defeated"`` / ``"phase1_defeated"``)
        are wrapped as ``{"status": <string>}``. Missing fields default to
        ``status="active"``.
        """
        raw = boss_progress.get(str(boundary))
        if raw is None:
            return {"status": "active"}
        if isinstance(raw, str):
            return {"status": raw}
        if isinstance(raw, dict):
            return dict(raw)  # caller-mutable copy
        return {"status": "active"}

    def _build_boss_info(
        self, discord_id: int, guild_id, tunnel: dict, boundary: int,
    ) -> dict:
        """Build the boss encounter payload for a boundary.

        Locks a specific boss for this tunnel at this tier (idempotent), then
        picks a dialogue line from ``BOSS_DIALOGUE_V2`` keyed on
        ``first_meet`` / ``after_<last_outcome>`` if available, falling back
        to the legacy v1 dialogue list. Tokens like ``{streak}`` are
        substituted via ``_render_boss_bark``.

        For the pinnacle (depth 300), uses the rotating PINNACLE_BOSSES
        pool and the per-phase title/archetype.

        Updates ``first_meet_seen`` so the first-meet line only fires once
        per delve.
        """
        if self._is_pinnacle_depth(boundary):
            return self._build_pinnacle_info(discord_id, guild_id, tunnel)

        boss = self._ensure_boss_locked(discord_id, guild_id, tunnel, boundary)
        attempts = tunnel.get("boss_attempts", 0) or 0

        boss_progress = json.loads(tunnel.get("boss_progress") or "{}")
        entry = self._read_boss_progress_entry(boss_progress, boundary)
        last_outcome = entry.get("last_outcome")
        first_meet_seen = bool(entry.get("first_meet_seen", False))

        # Choose dialogue slot.
        if not first_meet_seen:
            slot = "first_meet"
        elif last_outcome in ("defeated", "retreat", "scout", "close_win"):
            slot = f"after_{last_outcome}"
        else:
            slot = "first_meet"  # default to first-meet flavor when no history

        v1_fallback_list = boss.dialogue or BOSS_DIALOGUE.get(boundary, ["..."])
        v1_fallback = v1_fallback_list[min(attempts, len(v1_fallback_list) - 1)]
        line = self._pick_boss_dialogue_line(boss.boss_id, slot, v1_fallback)
        rendered = self._render_boss_bark(line, tunnel)

        # Mark first-meet seen so subsequent encounters use outcome-aware lines.
        if not first_meet_seen:
            entry["first_meet_seen"] = True
            # Preserve boss_id when present (multi-boss tier lookup).
            entry.setdefault("boss_id", boss.boss_id)
            boss_progress[str(boundary)] = entry
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                boss_progress=json.dumps(boss_progress),
            )
            tunnel["boss_progress"] = json.dumps(boss_progress)

        return {
            "boundary": boundary,
            "boss_id": boss.boss_id,
            "name": boss.name,
            "dialogue": rendered,
            "ascii_art": boss.ascii_art or BOSS_ASCII.get(boundary, ""),
            "luminosity_display": self._luminosity_combat_display(tunnel),
        }

    def _build_pinnacle_info(
        self, discord_id: int, guild_id, tunnel: dict,
    ) -> dict:
        """Build the pinnacle encounter payload (depth 300).

        Locks a pinnacle boss from the rotating pool on first encounter,
        then returns the current-phase title and a dialogue line from
        BOSS_DIALOGUE_V2. The 3-phase structure is persisted on the tunnel
        in ``pinnacle_phase`` (1..3).
        """
        pinnacle_id = self._ensure_pinnacle_locked(discord_id, guild_id, tunnel)
        pinnacle = PINNACLE_BOSSES[pinnacle_id]
        phase_idx = max(1, min(3, int(tunnel.get("pinnacle_phase", 1) or 1)))
        phase_def = pinnacle.phases[phase_idx - 1]

        boss_progress = json.loads(tunnel.get("boss_progress") or "{}")
        entry = self._read_boss_progress_entry(boss_progress, PINNACLE_DEPTH)
        last_outcome = entry.get("last_outcome")
        first_meet_seen = bool(entry.get("first_meet_seen", False))

        if not first_meet_seen:
            slot = "first_meet"
        elif last_outcome in ("defeated", "retreat", "scout", "close_win"):
            slot = f"after_{last_outcome}"
        else:
            slot = "first_meet"

        # Hand-authored fallback uses the phase transition_dialogue, then
        # the canonical first_meet pool from BOSS_DIALOGUE_V2[pinnacle_id].
        fallback = (
            phase_def.transition_dialogue[0]
            if phase_def.transition_dialogue
            else (BOSS_DIALOGUE_V2.get(pinnacle_id, {}).get("first_meet", ["..."])[0])
        )
        line = self._pick_boss_dialogue_line(pinnacle_id, slot, fallback)
        rendered = self._render_boss_bark(line, tunnel)

        if not first_meet_seen:
            entry["first_meet_seen"] = True
            entry.setdefault("boss_id", pinnacle_id)
            boss_progress[str(PINNACLE_DEPTH)] = entry
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                boss_progress=json.dumps(boss_progress),
            )
            tunnel["boss_progress"] = json.dumps(boss_progress)

        return {
            "boundary": PINNACLE_DEPTH,
            "boss_id": pinnacle_id,
            "name": phase_def.title,
            "dialogue": rendered,
            "ascii_art": pinnacle.ascii_art,
            "is_pinnacle": True,
            "phase": phase_idx,
            "phase_total": 3,
            "luminosity_display": self._luminosity_combat_display(tunnel),
        }

    def _pinnacle_foreshadow_line(self, tunnel: dict) -> str | None:
        """Return a subtle foreshadowing line for /dig info if the player
        has cleared all 7 tier bosses but not yet defeated the pinnacle.

        The line never names the depth — players discover the pinnacle by
        digging into it.
        """
        boss_progress = json.loads(tunnel.get("boss_progress") or "{}")
        all_tiers_cleared = (
            len(boss_progress) >= len(BOSS_BOUNDARIES)
            and all(
                (e.get("status") if isinstance(e, dict) else e) == "defeated"
                for b in BOSS_BOUNDARIES
                for e in (boss_progress.get(str(b)),)
                if e is not None
            )
        )
        if not all_tiers_cleared:
            return None
        pinnacle_entry = boss_progress.get(str(PINNACLE_DEPTH))
        pinnacle_status = (
            pinnacle_entry.get("status") if isinstance(pinnacle_entry, dict)
            else pinnacle_entry
        )
        if pinnacle_status == "defeated":
            return None
        return random.choice(PINNACLE_FORESHADOW_LINES)

    def _drop_pinnacle_relic(
        self, discord_id: int, guild_id, tunnel: dict, pinnacle_id: str,
    ) -> dict:
        """Roll and persist a pinnacle relic with 2 random stats.

        Returns the relic descriptor (name, stats, prestige_at_drop) for
        the embed. Stores it as a `dig_artifacts` row with a synthetic
        artifact_id of the form ``pinnacle:<base>:<suffix>:<stat1>:<stat2>``.
        Combat-affecting stats are decoded and folded into combat math via
        ``_apply_pinnacle_relic_stats`` when the relic is equipped.
        """
        prestige_level = tunnel.get("prestige_level", 0) or 0
        base_name = PINNACLE_RELIC_BASE_NAME[pinnacle_id]
        suffix = random.choice(PINNACLE_RELIC_SUFFIX_POOL)
        # Pick two distinct stats from the pool.
        stat_pool = list(PINNACLE_RELIC_STAT_POOL)
        random.shuffle(stat_pool)
        rolled_stats = stat_pool[:2]
        stat_ids = [s.id for s in rolled_stats]
        artifact_id = f"pinnacle:{base_name}:{suffix}:{stat_ids[0]}:{stat_ids[1]}"
        relic_db_id = self.dig_repo.add_artifact(
            discord_id, guild_id, artifact_id, is_relic=True,
        )
        return {
            "name": f"{base_name} of {suffix}",
            "stats": [s.label for s in rolled_stats],
            "stat_ids": stat_ids,
            "prestige_at_drop": prestige_level,
            "artifact_id": artifact_id,
            "db_id": relic_db_id,
        }

    def _apply_pinnacle_relic_stats(
        self,
        out: dict,
        loadout,
    ) -> dict:
        """Fold combat-relevant pinnacle relic stats into a stats dict.

        Pinnacle relics carry rolled stats encoded in their artifact_id
        (``pinnacle:<base>:<suffix>:<stat1>:<stat2>``). Stats that affect
        combat (player_hp, player_hit, boss_hit, boss_payout, boss_hp
        multiplier) are decoded and applied to ``out`` here. Dig/utility
        stats (jc_multiplier, cave_in_reduction, etc.) are surfaced via a
        separate aggregator at dig time.
        """
        for relic in loadout.relics or []:
            aid = relic.get("artifact_id", "") or ""
            if not aid.startswith("pinnacle:"):
                continue
            parts = aid.split(":")
            # ["pinnacle", base, suffix, stat1, stat2]
            if len(parts) < 5:
                continue
            for stat_id in parts[3:]:
                self._apply_pinnacle_stat(stat_id, out)
        return out

    def _apply_pinnacle_stat(self, stat_id: str, out: dict) -> None:
        """Apply a single pinnacle stat by id to a combat stats dict."""
        if stat_id == "hp_plus_1":
            out["player_hp"] = int(out.get("player_hp", 0)) + 1
        elif stat_id == "hit_plus_002":
            out["player_hit"] = float(out.get("player_hit", 0)) + 0.02
        elif stat_id == "boss_hit_minus":
            out["boss_hit"] = max(0.05, float(out.get("boss_hit", 0)) - 0.02)
        elif stat_id == "dmg_plus_per_100":
            # Applied lazily — this is the only stat that depends on at_boss.
            # The fight_boss path picks it up via _apply_pinnacle_depth_dmg.
            pass
        # Other stats (jc_multiplier, cave_in_reduction, lum_refill, etc.)
        # apply at dig-time, not boss-fight time. They're aggregated separately.

    def _generate_timed_challenge(self, kind: str) -> dict:
        """Generate a timed-input challenge for a pinnacle mechanic.

        Returns a dict with:
          - kind: "arithmetic" | "riddle"
          - question: the text to show the player
          - answer_canonical: a string used to evaluate (lowercased, stripped)
          - accepted: list of accepted lowercase answers (riddles can have synonyms)
          - time_window_seconds: how many seconds counts as "fast"
          - started_at: unix timestamp at challenge generation
        """
        now = int(time.time())
        if kind == "arithmetic":
            # (2-9) * (2-9) + (1-9), as the user spec'd. Always non-negative.
            a = random.randint(2, 9)
            b = random.randint(2, 9)
            c = random.randint(1, 9)
            answer = a * b + c
            return {
                "kind": "arithmetic",
                "question": f"{a} * {b} + {c} = ?",
                "answer_canonical": str(answer),
                "accepted": [str(answer)],
                "time_window_seconds": 20,
                "started_at": now,
            }
        if kind == "riddle":
            from domain.models.boss_mechanics import RIDDLE_POOL as _POOL
            text, accepted = random.choice(_POOL)
            return {
                "kind": "riddle",
                "question": text,
                "answer_canonical": accepted[0],
                "accepted": [a.lower().strip() for a in accepted],
                "time_window_seconds": 30,
                "started_at": now,
            }
        # Unknown kind — pretend timeout.
        return {
            "kind": kind, "question": "?", "answer_canonical": "",
            "accepted": [], "time_window_seconds": 0, "started_at": now,
        }

    def _evaluate_timed_answer(
        self, challenge: dict, answer_text: str, now: int,
    ) -> int:
        """Map a submitted answer + elapsed time to one of three options.

        Returns 0 for correct+fast, 1 for correct+slow, 2 for wrong/timeout.
        """
        if not challenge:
            return 2
        def _norm(s: str) -> str:
            return " ".join(unicodedata.normalize("NFKD", s or "").lower().split())
        cleaned = _norm(answer_text)
        accepted = [_norm(a) for a in challenge.get("accepted", [])]
        is_correct = bool(cleaned and cleaned in accepted)
        elapsed = max(0, int(now) - int(challenge.get("started_at", now)))
        within_window = elapsed <= int(challenge.get("time_window_seconds", 0))
        if is_correct and within_window:
            return 0
        if is_correct:
            return 1
        return 2

    def submit_timed_answer(
        self, discord_id: int, guild_id, answer_text: str,
    ) -> dict:
        """Player-submitted answer for an active timed-input pinnacle prompt.

        Reads the active duel state, evaluates the answer against the
        persisted challenge, and resumes the duel with the mapped option_idx.

        Uses ``claim_active_duel`` (atomic read-and-delete) so a second
        concurrent submission — e.g. a duplicate Discord modal ``on_submit``
        from a network retry — gets ``None`` and bails out instead of
        triggering a double resolution.
        """
        state_row = self.dig_repo.claim_active_duel(discord_id, guild_id)
        if state_row is None:
            return self._error("No active timed prompt to submit.")
        try:
            status_effects = json.loads(state_row["status_effects"] or "{}")
        except (json.JSONDecodeError, TypeError):
            status_effects = {}
        challenge = status_effects.get("timed_challenge")
        if not challenge:
            return self._error("Active prompt is not a timed-input challenge.")
        option_idx = self._evaluate_timed_answer(
            challenge, answer_text, int(time.time()),
        )
        result = self.resume_boss_duel(
            discord_id, guild_id, option_idx=option_idx, state_row=state_row,
        )
        # Surface what the player was being asked, plus the resolution outcome,
        # so the UI can show "you answered 47 (correct, in time)" or similar.
        if isinstance(result, dict) and result.get("success") is not False:
            result.setdefault("timed_challenge_resolution", {
                "option_idx": option_idx,
                "submitted": answer_text,
                "correct": option_idx in (0, 1),
                "in_time": option_idx == 0,
                "expected_answer": challenge.get("answer_canonical"),
            })
        return result

    def _pinnacle_dmg_per_100_count(self, loadout) -> int:
        """Count how many ``dmg_plus_per_100`` stats are equipped on
        pinnacle relics. The fight_boss path uses this to add
        ``count * (depth // 100)`` to player_dmg.
        """
        count = 0
        for relic in (loadout.relics if loadout else []) or []:
            aid = relic.get("artifact_id", "") or ""
            if not aid.startswith("pinnacle:"):
                continue
            parts = aid.split(":")
            if len(parts) >= 5:
                for stat_id in parts[3:]:
                    if stat_id == "dmg_plus_per_100":
                        count += 1
        return count

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

    def _luminosity_combat_display(self, tunnel: dict) -> str | None:
        """Return a one-line description of the luminosity combat penalty,
        or ``None`` when the player is at Bright (no penalty to surface).

        Used by the boss UI to make the otherwise-invisible accuracy/dmg
        penalty discoverable so players can choose to retreat and refill
        before pulling the trigger on a fight they can't actually win.
        """
        luminosity = self._get_luminosity(tunnel)
        hit_offset, dmg_bonus = _luminosity_combat_penalty(luminosity)
        if hit_offset == 0 and dmg_bonus == 0:
            return None
        level = self._get_luminosity_level(luminosity).replace("_", " ").title()
        parts = [f"{int(hit_offset * 100)}% hit"]
        if dmg_bonus:
            parts.append(f"+{dmg_bonus} boss dmg")
        return f"Luminosity: **{level} ({luminosity})** — {', '.join(parts)}"

    def _apply_luminosity_drain(self, discord_id: int, guild_id, tunnel: dict, layer_name: str) -> dict:
        """Apply slow refill from last_lum_update_at, then drain for this dig.

        Refill rate is ``LUMINOSITY_REFILL_PER_DAY`` (default 20) per real-world
        day, computed continuously: ``floor(hours_elapsed * REFILL_PER_DAY / 24)``.
        The old daily snap-back to 100 has been removed — luminosity now
        carries across sessions and only recovers slowly without intervention
        (use Torch / Spore Cloak / events for faster recovery).

        Returns dict with luminosity_before, luminosity_after, level, drained.
        """
        now = int(time.time())
        luminosity = self._get_luminosity(tunnel)

        # Slow refill: recover floor(hours * REFILL/24) since last update.
        # ``last_lum_update_at`` defaults to ``now`` for fresh tunnels so the
        # first dig doesn't get a free refill from time-zero.
        last_update = tunnel.get("last_lum_update_at") or now
        try:
            last_update = int(last_update)
        except (TypeError, ValueError):
            last_update = now
        hours_elapsed = max(0.0, (now - last_update) / 3600.0)
        refill = int(hours_elapsed * (LUMINOSITY_REFILL_PER_DAY / 24.0))
        if refill > 0:
            luminosity = min(LUMINOSITY_MAX, luminosity + refill)

        before = luminosity
        drain = LUMINOSITY_DRAIN_PER_DIG.get(layer_name, 0)
        # Frostforged / Void-Touched pickaxe: -25% luminosity drain
        pickaxe_tier = self._get_active_pickaxe_tier(discord_id, guild_id, tunnel)
        if pickaxe_tier >= 5:  # Frostforged or better
            drain = max(0, drain - drain // 4)
        # Past the pinnacle the deep grows hungry — drain ramps linearly
        # toward the hard cap, applying pressure to prestige.
        depth = int(tunnel.get("depth", 0) or 0)
        if depth > LUMINOSITY_DEEP_DRAIN_START_DEPTH:
            drain += (depth - LUMINOSITY_DEEP_DRAIN_START_DEPTH) // (
                LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP
            )
        luminosity = max(0, luminosity - drain)

        # Persist both luminosity and the timestamp so subsequent digs compute
        # refill from the correct anchor.
        self.dig_repo.update_tunnel(
            discord_id,
            guild_id,
            luminosity=luminosity,
            last_lum_update_at=now,
        )
        tunnel["luminosity"] = luminosity
        tunnel["last_lum_update_at"] = now

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
        bosses_defeated = sum(
            1 for v in boss_progress.values()
            if (v.get("status") if isinstance(v, dict) else v) == "defeated"
        )
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
        """Decay is disabled. Returns a no-op result for all callers."""
        return {"decayed": False, "amount": 0, "reason": None}

    # ------------------------------------------------------------------
    # Core Dig
    # ------------------------------------------------------------------

    def _build_parked_boss_return(
        self, tunnel: dict, discord_id: int, guild_id, decay_info
    ) -> dict | None:
        """If the tunnel is already at a defeated-eligible boss boundary, return
        a boss-encounter result dict so /dig stops here without charging cooldown
        or paid fees. Returns ``None`` if the tunnel is not parked at a boundary.

        ``last_dig_at`` is intentionally left untouched: the cooldown timer
        should continue ticking from the last real dig, not reset every time
        the player reopens the boss view.
        """
        depth_before = tunnel.get("depth", 0)
        boss_progress_early = self._get_boss_progress(tunnel)
        at_boss_early = self._at_boss_boundary(depth_before, boss_progress_early)
        if at_boss_early is None:
            return None

        inv = self.dig_repo.get_inventory(discord_id, guild_id)
        has_lantern_early = any(i.get("item_type") == "lantern" for i in inv)
        boss_info = self._build_boss_info(discord_id, guild_id, tunnel, at_boss_early)
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
            boss_info=boss_info,
            has_lantern=has_lantern_early,
            event=None,
            artifact=None,
            achievements=[],
            is_first_dig=False,
            items_used=[],
            items_used_ids=[],
            pickaxe_tier=self._get_active_pickaxe_tier(discord_id, guild_id, tunnel),
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

        # 2b. If the player is already parked at a boss boundary, surface the
        #     boss encounter immediately — cooldown and paid-dig gates would
        #     block access to the Fight button. Re-opening the view awards no
        #     JC, which closes the original farm exploit.
        if not is_first_dig:
            boss_progress_check = self._get_boss_progress(tunnel)
            if self._at_boss_boundary(depth_before, boss_progress_check) is not None:
                parked_return = self._build_parked_boss_return(
                    tunnel, discord_id, guild_id, decay_info
                )
                if parked_return is not None:
                    return parked_return

        # 2c. Hard cap: the deep refuses to yield further. Block dig
        #     before any cost or cooldown is consumed so the player can
        #     prestige cleanly.
        if not is_first_dig and depth_before >= PRESTIGE_HARD_CAP:
            return {
                "success": False,
                "error": "The earth refuses to yield further. The path beyond demands ascension.",
                "hard_cap": True,
            }

        # 3. Cooldown / paid dig check — normal digs only, parked players
        #    short-circuited above.
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

                # Debit for paid dig: balance + paid-day counter commit
                # together so a crash can't take the cost without
                # incrementing paid_digs_today (or vice versa).
                self.dig_repo.atomic_tunnel_balance_update(
                    discord_id, guild_id,
                    balance_delta=-paid_dig_cost,
                    tunnel_updates={
                        "paid_dig_date": today,
                        "paid_digs_today": paid_count + 1,
                    },
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

        pickaxe_tier = self._get_active_pickaxe_tier(discord_id, guild_id, tunnel)
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
            block_loss = random.randint(CAVE_IN_BLOCK_LOSS_MIN, CAVE_IN_BLOCK_LOSS_MAX)
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

            # Accumulate all cave-in writes into one atomic commit:
            # thick_skin_date, second_wind buff, injury_state, depth delta,
            # counter bump, last_dig_at, cave-in loot credit, medical bill
            # debit, and the audit log all flip together.
            cave_in_tunnel_updates: dict = {
                "depth": new_depth,
                "total_digs": (tunnel.get("total_digs", 0) or 0) + 1,
                "last_dig_at": now,
            }
            cave_in_balance_delta = 0

            if thick_skin_saved:
                cave_in_tunnel_updates["thick_skin_date"] = today

            # Mutation: cave_in_loot — chance to drop JC on cave-in
            cave_in_jc = 0
            loot_chance = mutation_fx.get("cave_in_loot_chance", 0)
            if loot_chance > 0 and random.random() < loot_chance:
                loot_min = int(mutation_fx.get("cave_in_loot_min", 1))
                loot_max = int(mutation_fx.get("cave_in_loot_max", 3))
                cave_in_jc = random.randint(loot_min, loot_max)
                cave_in_balance_delta += cave_in_jc

            # Mutation: second_wind — flag for next dig advance bonus
            if mutation_fx.get("post_cave_in_advance"):
                cave_in_tunnel_updates["temp_buffs"] = json.dumps({
                    "id": "second_wind", "name": "Second Wind",
                    "digs_remaining": 1,
                    "effect": {"advance_bonus": int(mutation_fx["post_cave_in_advance"])},
                })

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
                cave_in_tunnel_updates["injury_state"] = json.dumps(
                    {"type": "slower_cooldown", "digs_remaining": 2 + injury_bonus}
                )
            elif consequence_roll < 0.6:
                # Injury: reduced advance
                cave_in_detail = {
                    "type": "injury",
                    "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're injured (reduced digging for {3 + injury_bonus} digs).",
                }
                cave_in_tunnel_updates["injury_state"] = json.dumps(
                    {"type": "reduced_advance", "digs_remaining": 3 + injury_bonus}
                )
            else:
                # Medical bill (capped at current balance to prevent negative)
                med_cost = random.randint(3, 9)
                balance = self.player_repo.get_balance(discord_id, guild_id)
                med_cost = min(med_cost, max(0, balance))
                cave_in_balance_delta -= med_cost
                cave_in_detail = {
                    "type": "medical_bill",
                    "block_loss": block_loss,
                    "jc_lost": med_cost,
                    "message": f"Cave-in! Lost {block_loss} blocks and paid {med_cost} JC in medical bills.",
                }

            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=cave_in_balance_delta,
                tunnel_updates=cave_in_tunnel_updates,
                log_detail={
                    "cave_in": True, "block_loss": block_loss,
                    "detail": cave_in_detail,
                    "depth_before": depth_before, "depth_after": new_depth,
                },
                log_action_type="dig",
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
            boss_info = self._build_boss_info(discord_id, guild_id, tunnel, next_boss)

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

        # 14. Check milestones (with ascension milestone multiplier).
        # Only award milestones that extend the tunnel's all-time high
        # so boss cave-ins cannot be farmed by re-crossing boundaries.
        milestone_bonus = 0
        milestone_mult = 1.0 + ascension.get("milestone_multiplier", 0)
        prev_max_depth = tunnel.get("max_depth", 0) or 0
        milestone_floor = max(depth_before, prev_max_depth)
        for m_depth, m_reward in MILESTONES.items():
            if milestone_floor < m_depth <= new_depth:
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
            "Dirt": 0.25, "Stone": 0.25, "Crystal": 0.30, "Magma": 0.30,
            "Abyss": 0.35, "Fungal Depths": 0.42, "Frozen Core": 0.35, "The Hollow": 0.50,
        }
        event_chance = event_rates.get(layer_name, 0.24)
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
        # Void Bait: double event chance while charges remain. Decrement is
        # folded into the final atomic commit below so a crash can't burn a
        # void-bait charge without the dig committing.
        void_bait_digs = tunnel.get("void_bait_digs", 0) or 0
        void_bait_charge_used = void_bait_digs > 0
        if void_bait_charge_used:
            event_chance *= 2.0
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

        # 19. Final commit: tunnel state flip (incl. void-bait decrement if
        # applicable, depth, max_depth, counters, streak, run counters) +
        # JC credit + audit log — one BEGIN IMMEDIATE. A crash can no
        # longer move the player's depth without crediting the JC (or vice
        # versa), and the void-bait charge can't be burned without a dig
        # committing.
        run_jc = (tunnel.get("current_run_jc", 0) or 0) + jc_earned
        run_artifacts = (tunnel.get("current_run_artifacts", 0) or 0) + (1 if artifact else 0)
        run_events_count = (tunnel.get("current_run_events", 0) or 0) + (1 if event else 0)
        final_tunnel_updates: dict = {
            "depth": new_depth,
            "max_depth": max(prev_max_depth, new_depth),
            "total_digs": total_digs,
            "last_dig_at": now,
            "total_jc_earned": (tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
            "streak_days": streak,
            "streak_last_date": today,
            "current_run_jc": run_jc,
            "current_run_artifacts": run_artifacts,
            "current_run_events": run_events_count,
        }
        if void_bait_charge_used:
            final_tunnel_updates["void_bait_digs"] = void_bait_digs - 1

        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=jc_earned,
            tunnel_updates=final_tunnel_updates,
            log_detail={
                "advance": advance, "jc": jc_earned,
                "depth_before": depth_before, "depth_after": new_depth,
                "boss_encounter": boss_encounter,
                "cave_in": False,
                "corruption": corruption["id"] if corruption else None,
            },
            log_action_type="dig",
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

        # Parked-at-boss short-circuit: surface the encounter before the
        # cooldown gate so Fight/Retreat buttons are always reachable.
        if not is_first_dig:
            boss_progress_check = self._get_boss_progress(tunnel)
            if self._at_boss_boundary(depth_before, boss_progress_check) is not None:
                parked_return = self._build_parked_boss_return(
                    tunnel, discord_id, guild_id, decay_info,
                )
                if parked_return is not None:
                    return parked_return, None

        # Cooldown / paid dig check — normal digs only.
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

        pickaxe_tier = self._get_active_pickaxe_tier(discord_id, guild_id, tunnel)
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
            "Dirt": 0.25, "Stone": 0.25, "Crystal": 0.30, "Magma": 0.30,
            "Abyss": 0.35, "Fungal Depths": 0.42, "Frozen Core": 0.35,
            "The Hollow": 0.50,
        }
        event_chance = event_rates.get(layer_name, 0.24)
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

        # ── Social Modifiers ──────────────────────────────────────
        # Cheers, help, and sabotage create karmic feedback loops.
        cheer_advance_bonus = 0
        help_jc_bonus = 0
        sabotage_karma = 0.0
        sabotage_sympathy = 0.0
        help_event_bonus = 0.0

        # Active cheers → advance bonus (+1 per cheer, max +3)
        active_cheers = [
            c for c in self._get_cheers(tunnel)
            if c.get("expires_at", 0) > now
        ]
        cheer_advance_bonus = min(len(active_cheers), 3)
        advance_max += cheer_advance_bonus

        # Recent social actions (single DB call, last 24h)
        recent_social = self.dig_repo.get_recent_actions(
            discord_id, guild_id, 20, hours=24,
        )
        help_given = [
            a for a in recent_social
            if a.get("action_type") == "help" and a.get("actor_id") == discord_id
        ]
        help_received = [
            a for a in recent_social
            if a.get("action_type") == "help" and a.get("target_id") == discord_id
        ]
        sabotage_given = [
            a for a in recent_social
            if a.get("action_type") == "sabotage" and a.get("actor_id") == discord_id
        ]
        sabotage_received = [
            a for a in recent_social
            if a.get("action_type") == "sabotage" and a.get("target_id") == discord_id
        ]

        # Helped someone recently → +1 jc_min (generosity rewarded)
        if help_given:
            help_jc_bonus = 1
            jc_min += help_jc_bonus

        # Sabotaged someone recently → +3% cave-in per sabotage (max +9%)
        sabotage_karma = min(len(sabotage_given), 3) * 0.03
        cave_in_chance += sabotage_karma

        # Been sabotaged recently → -3% cave-in (sympathy)
        if sabotage_received:
            sabotage_sympathy = 0.03
            cave_in_chance -= sabotage_sympathy

        # Been helped recently → +5% event chance (allied passages)
        if help_received:
            help_event_bonus = 0.05
            event_chance += help_event_bonus

        # Re-clamp after social modifiers
        cave_in_chance = max(0.01, cave_in_chance)
        event_chance = min(event_chance, 0.75)

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
            "cheer_advance_bonus": cheer_advance_bonus,
            "help_jc_bonus": help_jc_bonus,
            "sabotage_karma": sabotage_karma,
            "sabotage_sympathy": sabotage_sympathy,
            "help_event_bonus": help_event_bonus,
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
            block_loss = random.randint(CAVE_IN_BLOCK_LOSS_MIN, CAVE_IN_BLOCK_LOSS_MAX)
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
                med_cost = random.randint(3, 9)
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
            boss_info = self._build_boss_info(discord_id, guild_id, tunnel, next_boss)
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

        # Milestones (anti-farm: only award on depths that extend all-time high).
        milestone_bonus = 0
        milestone_mult = 1.0 + p["ascension"].get("milestone_multiplier", 0)
        prev_max_depth = tunnel.get("max_depth", 0) or 0
        milestone_floor = max(depth_before, prev_max_depth)
        for m_depth, m_reward in MILESTONES.items():
            if milestone_floor < m_depth <= new_depth:
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
            max_depth=max(prev_max_depth, new_depth),
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
                med_cost = outcome.get("cave_in_jc_lost", 5)
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
                boss_info = self._build_boss_info(discord_id, guild_id, tunnel, next_boss)
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
                # Look up name from pool
                name = artifact_id
                for pool_item in ARTIFACT_POOL:
                    if pool_item["id"] == artifact_id:
                        name = pool_item["name"]
                        break
                relics.append({
                    "id": artifact_id,
                    "db_id": a.get("id"),
                    "name": name,
                    "equipped": a.get("equipped", 0),
                })
        return relics

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

        # Cap at boss boundary
        boss_progress = self._get_boss_progress(target_tunnel)
        next_boss = self._next_boss_boundary(target_depth, boss_progress)
        if next_boss is not None and target_depth + advance >= next_boss:
            advance = max(0, next_boss - 1 - target_depth)

        new_depth = target_depth + advance

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
            helper_reward=1,
            create_helper_tunnel_name=None if helper_tunnel else self.generate_tunnel_name(),
            log_detail={
                "target_id": target_id, "advance": advance,
                "target_depth_before": target_depth, "target_depth_after": new_depth,
            },
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
            trap_steal = cost * 2
            actor_tunnel = self.dig_repo.get_tunnel(actor_id, guild_id)
            actor_loss = random.randint(3, 5)

            self.dig_repo.atomic_sabotage(
                actor_id=actor_id,
                target_id=target_id,
                guild_id=guild_id,
                target_depth_delta=0,
                actor_jc_cost=trap_steal,
                target_jc_credit=cost,
                actor_depth_delta=-actor_loss if actor_tunnel else 0,
                clear_target_trap=True,
                log_detail={
                    "target_id": target_id, "trap_triggered": True,
                    "jc_lost": trap_steal, "blocks_lost": actor_loss,
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

        self.dig_repo.atomic_sabotage(
            actor_id=actor_id,
            target_id=target_id,
            guild_id=guild_id,
            target_depth_delta=-damage,
            actor_jc_cost=cost,
            revenge={
                "target": actor_id,
                "type": revenge["type"],
                "until": revenge["expires_at"],
            },
            log_detail={
                "target_id": target_id, "damage": damage, "cost": cost,
                "trap_triggered": False,
            },
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
            "pinnacle_foreshadow": pinnacle_foreshadow,
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

        boss_info = self._build_boss_info(discord_id, guild_id, tunnel, at_boss)
        attempts = tunnel.get("boss_attempts", 0) or 0

        return self._ok(
            boundary=at_boss,
            boss_id=boss_info["boss_id"],
            boss_name=boss_info["name"],
            dialogue=boss_info["dialogue"],
            ascii_art=boss_info["ascii_art"],
            attempts=attempts,
            options=["cautious", "bold", "reckless"],
            luminosity_display=self._luminosity_combat_display(tunnel),
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

        # Pinnacle has its own 3-phase resolver — different boss data
        # structure and "always 3 phases regardless of prestige" rules.
        if self._is_pinnacle_depth(at_boss):
            return self._fight_pinnacle(discord_id, guild_id, tunnel, risk_tier, wager)

        # ---- Multi-round HP duel ---------------------------------------
        # Each round the player attacks first; if the boss survives, it
        # counterattacks. Whichever side reaches 0 HP first loses.
        base_stats = BOSS_DUEL_STATS.get(risk_tier, BOSS_DUEL_STATS["bold"])
        # Fold the player's equipped gear into the base risk-tier stats
        # before any depth/prestige/cheer/wager modifiers are applied.
        # ``_apply_gear_to_combat`` already clamps player_hit and floors
        # boss_hit; the depth/prestige penalties below stack on top.
        loadout = self._get_loadout(discord_id, guild_id)
        stats = self._apply_gear_to_combat(base_stats, loadout)
        tier_index = {"cautious": 0, "bold": 1, "reckless": 2}.get(risk_tier, 1)
        payouts = BOSS_PAYOUTS.get(at_boss, (2.0, 3.0, 6.0))
        multiplier = payouts[tier_index] if tier_index < len(payouts) else 2.0

        prestige_level = tunnel.get("prestige_level", 0) or 0

        # Cheer bonus (existing mechanic: +5% accuracy per cheer, cap 3 cheers).
        cheers = self._get_cheers(tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        # Phase accuracy penalty: phase 2 (status phase1_defeated) reads
        # BOSS_PHASE2; phase 3 (status phase2_defeated) reads BOSS_PHASE3.
        phase2_penalty = 0.0
        _phase_status = boss_progress.get(str(at_boss))
        if _phase_status == "phase1_defeated" and at_boss in BOSS_PHASE2:
            phase2_penalty = abs(BOSS_PHASE2[at_boss].win_odds_penalty)
        elif _phase_status == "phase2_defeated" and at_boss in BOSS_PHASE3:
            phase2_penalty = abs(BOSS_PHASE3[at_boss].win_odds_penalty)

        # Lock the boss to know its archetype for stat scaling.
        boss_def = self._ensure_boss_locked(discord_id, guild_id, tunnel, at_boss)
        active_boss_id = boss_def.boss_id

        # Echo weakening: if another guildmate has killed this boss within
        # the last 24h, the boss comes in at -25% HP and pays -30%. The
        # original killer is exempt so re-runs can't farm their own discount.
        active_echo = self.dig_repo.get_active_boss_echo(guild_id, active_boss_id)
        echo_applied = bool(
            active_echo
            and active_echo.get("killer_discord_id") != discord_id
        )

        # Apply boss-side scaling (archetype + depth + prestige + echo).
        scaled = self._scale_boss_stats(
            stats,
            boss_id=active_boss_id,
            at_boss=at_boss,
            prestige_level=prestige_level,
            echo_applied=echo_applied,
        )
        fresh_boss_hp = int(scaled["boss_hp"])
        # Carry over persisted HP from prior unfinished engagements with regen.
        boss_hp, boss_hp_max = self._resolve_persisted_boss_hp(
            boss_progress, at_boss, fresh_boss_hp, now,
        )
        boss_hit_chance = float(scaled["boss_hit"])
        boss_dmg = int(scaled["boss_dmg"])

        # Luminosity combat penalty (Dim/Dark/Pitch reduce player hit; Pitch buffs boss dmg).
        lum_value = self._get_luminosity(tunnel)
        lum_hit_offset, lum_dmg_bonus = _luminosity_combat_penalty(lum_value)
        boss_dmg += lum_dmg_bonus

        # Player-side hit calc: tier+prestige penalty (from lookup tables),
        # phase2 penalty, cheers, luminosity penalty, free-fight mod,
        # then floor/ceiling.
        tier_key = at_boss if at_boss in BOSS_TIER_BONUS else max(
            (k for k in BOSS_TIER_BONUS if k <= at_boss), default=25,
        )
        depth_hit_penalty = BOSS_TIER_BONUS[tier_key]["pen"]
        prestige_hit_penalty = BOSS_PRESTIGE_BONUS.get(
            prestige_level, BOSS_PRESTIGE_BONUS[max(BOSS_PRESTIGE_BONUS)],
        )["pen"]
        player_hit = (
            stats["player_hit"]
            - depth_hit_penalty - prestige_hit_penalty - phase2_penalty
            + cheer_bonus
            + lum_hit_offset
        )
        if wager == 0:
            player_hit *= BOSS_FREE_FIGHT_ACCURACY_MOD
        player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))

        player_hp = int(stats["player_hp"])
        player_dmg = int(stats["player_dmg"])

        # Estimate actual win probability via Monte Carlo on the entry
        # stats so the returned ``win_chance`` matches what ``scout_boss``
        # would show — per-round hit rate is not the same as duel win rate.
        win_chance = _approx_duel_win_prob(
            player_hp=player_hp,
            boss_hp=boss_hp,
            player_hit=player_hit,
            player_dmg=player_dmg,
            boss_hit=boss_hit_chance,
            boss_dmg=boss_dmg,
        )

        round_log: list[dict] = []
        won: bool | None = None
        for round_num in range(1, BOSS_ROUND_CAP + 1):
            entry: dict = {"round": round_num}
            player_roll = random.random() < player_hit
            if player_roll:
                boss_hp -= player_dmg
            entry["player_hit"] = player_roll
            entry["boss_hp"] = max(0, boss_hp)
            if boss_hp <= 0:
                won = True
                round_log.append(entry)
                break
            boss_roll = random.random() < boss_hit_chance
            if boss_roll:
                player_hp -= boss_dmg
            entry["boss_hit"] = boss_roll
            entry["player_hp"] = max(0, player_hp)
            round_log.append(entry)
            if player_hp <= 0:
                won = False
                break
        else:
            # Round cap hit without a decision: the boss wins. Players who
            # can't land a killing blow in BOSS_ROUND_CAP rounds have
            # clearly lost the initiative. In realistic play with the
            # default hit rates this branch is essentially unreachable
            # (Cautious at 0.65 hit has <1-in-40k chance of missing 20
            # times). It matters for deterministic tests that pin
            # ``random.random`` to extreme values.
            won = False

        # Wear-and-tear: every equipped gear piece loses 1 durability per
        # fight (win or lose). Anything that just hit zero gets reported
        # back so the embed can announce it.
        broken_ids = self.dig_repo.tick_gear_durability(discord_id, guild_id)
        gear_broken_names: list[str] = []
        if broken_ids:
            name_by_id: dict[int, str] = {}
            for piece in (loadout.weapon, loadout.armor, loadout.boots):
                if piece is not None:
                    name_by_id[piece.id] = piece.tier_def.name
            gear_broken_names = [name_by_id.get(i, "a piece of gear") for i in broken_ids]

        boss_name = BOSS_NAMES.get(at_boss, "Unknown Boss")
        attempts = (tunnel.get("boss_attempts", 0) or 0) + 1

        # Apply ascension boss payout modifier (P4+)
        ascension = self._get_ascension_effects(prestige_level)
        boss_payout_mult = 1.0 + ascension.get("boss_payout_multiplier", 0)

        if won:
            # Phase gating: P2+ unlocks phase 2 (was P4); P5+ AND tier>=100 unlocks phase 3.
            # The phase event pool is also rolled at the transition for flavor.
            current_status = boss_progress.get(str(at_boss), "active")
            phase2_min_p = int(BOSS_PHASES.get("phase_2_min_prestige", 2))
            phase3_min_p = int(BOSS_PHASES.get("phase_3_min_prestige", 5))
            phase3_min_tier = int(BOSS_PHASES.get("phase_3_min_tier", 100))

            needs_phase2 = (
                prestige_level >= phase2_min_p
                and at_boss in BOSS_PHASE2
                and current_status == "active"
            )
            needs_phase3 = (
                prestige_level >= phase3_min_p
                and at_boss >= phase3_min_tier
                and at_boss in BOSS_PHASE3
                and current_status == "phase1_defeated"
            )

            if needs_phase2 or needs_phase3:
                # Phase transition — boss transforms, fight again. Tunnel
                # update + audit log commit together via atomic helper.
                next_status = "phase1_defeated" if needs_phase2 else "phase2_defeated"
                phase_def = BOSS_PHASE2[at_boss] if needs_phase2 else BOSS_PHASE3[at_boss]
                next_phase_num = 2 if needs_phase2 else 3
                # Roll an environmental transition event for flavor (mechanical
                # effects on the next round are TODO — for now only the flavor
                # surfaces in the embed).
                phase_event = random.choice(PHASE_TRANSITION_EVENTS)
                boss_progress[str(at_boss)] = next_status
                self.dig_repo.atomic_tunnel_balance_update(
                    discord_id, guild_id,
                    tunnel_updates={
                        "boss_progress": json.dumps(boss_progress),
                        "boss_attempts": attempts,
                        "last_dig_at": now,
                    },
                    log_detail={
                        "boundary": at_boss, "won": True, "risk": risk_tier,
                        "phase": next_phase_num - 1, "wager": wager, "rounds": round_log,
                    },
                    log_action_type="boss_fight",
                )

                p_dialogue = phase_def.dialogue[min(attempts - 1, len(phase_def.dialogue) - 1)]

                return self._ok(
                    won=True,
                    phase=next_phase_num - 1,
                    phase2_incoming=needs_phase2,
                    phase3_incoming=needs_phase3,
                    boss_name=boss_name,
                    phase2_name=phase_def.name,
                    phase2_title=phase_def.title,
                    phase_event_flavor=phase_event.flavor,
                    phase_event_description=phase_event.description,
                    boundary=at_boss,
                    risk_tier=risk_tier,
                    win_chance=round(win_chance, 2),
                    jc_delta=0,
                    payout=0,
                    new_depth=depth,
                    dialogue=p_dialogue,
                    achievements=[],
                    round_log=round_log,
                    echo_applied=echo_applied,
                    echo_killer_id=active_echo.get("killer_discord_id") if echo_applied else None,
                    gear_broken=gear_broken_names,
                    gear_drop=None,
                )

            # Full victory (or phase 2 already cleared)
            new_depth = at_boss
            echo_payout_mult = 0.7 if echo_applied else 1.0
            base_jc = int(wager * multiplier) if wager > 0 else random.randint(8, 18)
            jc_delta = int(base_jc * boss_payout_mult * echo_payout_mult)

            # Persist outcome for future dialogue picks. close_win signals when
            # the player just barely won — the boss responds differently.
            outcome_label = "close_win" if win_chance < 0.6 else "defeated"
            boss_progress[str(at_boss)] = {
                "status": "defeated",
                "last_outcome": outcome_label,
                "first_meet_seen": True,
                "boss_id": active_boss_id,
                "hp_remaining": 0,
                "hp_max": boss_hp_max,
                "last_engaged_at": int(now),
            }
            prev_max_depth = tunnel.get("max_depth", 0) or 0

            # Compute stat point award (pure) so it can fold into the atomic
            # tunnel write instead of being a second UPDATE.
            tunnel_updates = {
                "depth": new_depth,
                "max_depth": max(prev_max_depth, new_depth),
                "boss_progress": json.dumps(boss_progress),
                "boss_attempts": 0,
                "cheer_data": None,  # Clear cheers
                "last_dig_at": now,
            }
            awarded_bosses = self._get_stat_boss_awards(tunnel)
            stat_point_awarded = at_boss not in awarded_bosses
            if stat_point_awarded:
                new_awarded = sorted(set(awarded_bosses + [at_boss]))
                current_points = max(
                    DIG_STARTING_STAT_POINTS,
                    int(tunnel.get("stat_points") or DIG_STARTING_STAT_POINTS),
                )
                tunnel_updates["stat_points"] = current_points + DIG_BOSS_STAT_POINT_BONUS
                tunnel_updates["stat_boss_awards"] = json.dumps(new_awarded)
                # Keep the in-memory tunnel dict consistent for downstream
                # achievement checks that pass ``{**tunnel, ...}``.
                tunnel["stat_points"] = tunnel_updates["stat_points"]
                tunnel["stat_boss_awards"] = tunnel_updates["stat_boss_awards"]

            if wager > 0:
                payout_delta = int(wager * (multiplier * boss_payout_mult * echo_payout_mult - 1))
            else:
                payout_delta = jc_delta

            # Tunnel flip + JC payout + boss-echo refresh + audit log all
            # commit in one BEGIN IMMEDIATE. A crash can no longer pay out
            # without clearing the boss (or vice versa).
            self.dig_repo.atomic_boss_full_victory(
                discord_id=discord_id,
                guild_id=guild_id,
                jc_delta=payout_delta,
                tunnel_updates=tunnel_updates,
                boss_echo_boss_id=active_boss_id,
                boss_echo_depth=at_boss,
                boss_echo_window_seconds=24 * 3600,
                log_detail={
                    "boundary": at_boss, "won": True, "risk": risk_tier,
                    "wager": wager, "jc_delta": jc_delta,
                    "stat_point_awarded": stat_point_awarded,
                    "echo_applied": echo_applied,
                    "rounds": round_log,
                },
            )

            # Achievement check runs in its own txns (reads + conditional
            # inserts + own JC rewards). A failure here leaves the boss
            # cleared but the achievement row not inserted; it can be
            # awarded on the next relevant check.
            achievements = self.check_achievements(
                discord_id, guild_id,
                {**tunnel, "depth": new_depth},
                {"action": "boss_win", "boundary": at_boss, "boss_progress": boss_progress},
            )

            dialogue_list = BOSS_DIALOGUE.get(at_boss, ["..."])
            defeat_msg = dialogue_list[-1] if dialogue_list else "Defeated!"

            # Roll a possible gear drop on the full kill. Phase-1 transitions
            # do NOT roll — only completed kills.
            gear_drop = self._maybe_drop_gear(discord_id, guild_id, at_boss)


            return self._ok(
                won=True,
                phase=(
                    3 if current_status == "phase2_defeated"
                    else 2 if current_status == "phase1_defeated"
                    else None
                ),
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
                round_log=round_log,
                echo_applied=echo_applied,
                echo_killer_id=active_echo.get("killer_discord_id") if echo_applied else None,
                gear_broken=gear_broken_names,
                gear_drop=gear_drop,
                luminosity_display=self._luminosity_combat_display(tunnel),
            )
        else:
            knockback = random.randint(8, 16)
            new_depth = max(0, depth - knockback)
            jc_delta = -wager if wager > 0 else 0

            # Persist post-fight boss HP so soften-and-retreat strategies work.
            # Mutates boss_progress in place to a dict with hp_remaining/last_engaged_at.
            self._persist_boss_hp_after_fight(
                boss_progress, at_boss, active_boss_id,
                ending_hp=max(0, boss_hp), hp_max=boss_hp_max,
                won=False, outcome="loss", now=now,
            )

            # Tunnel knockback + wager forfeit + audit log commit together.
            # The old flow could forfeit the wager without recording the
            # knockback (or vice versa) on a crash.
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=-wager if wager > 0 else 0,
                tunnel_updates={
                    "depth": new_depth,
                    "boss_progress": json.dumps(boss_progress),
                    "boss_attempts": attempts,
                    "cheer_data": None,     # clear cheers on defeat
                    "last_dig_at": now,
                },
                log_detail={
                    "boundary": at_boss, "won": False, "risk": risk_tier,
                    "wager": wager, "knockback": knockback,
                    "rounds": round_log, "boss_hp_remaining": max(0, boss_hp),
                },
                log_action_type="boss_fight",
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
                boss_hp_remaining=max(0, boss_hp),
                boss_hp_max=boss_hp_max,
                dialogue=f"{boss_name} sends you flying back {knockback} blocks!",
                achievements=[],
                round_log=round_log,
                echo_applied=echo_applied,
                echo_killer_id=active_echo.get("killer_discord_id") if echo_applied else None,
                gear_broken=gear_broken_names,
                gear_drop=None,
                luminosity_display=self._luminosity_combat_display(tunnel),
            )

    # =====================================================================
    # Pinnacle boss resolver
    # =====================================================================
    # The pinnacle is a single 3-phase fight at PINNACLE_DEPTH. Each phase
    # uses a distinct archetype (per PINNACLE_BOSSES[id].phases). Persisted
    # HP carries between phases. Defeating phase 3 marks the pinnacle
    # ``defeated`` and drops a unique relic with 2 random rolls.

    def _fight_pinnacle(
        self,
        discord_id: int,
        guild_id,
        tunnel: dict,
        risk_tier: str,
        wager: int,
    ) -> dict:
        """Resolve one phase of the pinnacle fight.

        On phase 1/2 win → advance pinnacle_phase, return phase-incoming
        response (with the next phase's transition_dialogue from the
        rolling event pool surfaced as flavor).

        On phase 3 win → mark pinnacle defeated in boss_progress, drop a
        pinnacle relic, return full-victory response.

        On any phase loss → persist boss HP, knockback the player, return
        loss response.
        """
        now = int(time.time())
        depth = tunnel.get("depth", 0)
        boss_progress = self._get_boss_progress(tunnel)

        pinnacle_id = self._ensure_pinnacle_locked(discord_id, guild_id, tunnel)
        pinnacle = PINNACLE_BOSSES[pinnacle_id]
        phase_idx = max(1, min(3, int(tunnel.get("pinnacle_phase", 1) or 1)))
        phase_def = pinnacle.phases[phase_idx - 1]

        prestige_level = tunnel.get("prestige_level", 0) or 0

        base_stats = BOSS_DUEL_STATS.get(risk_tier, BOSS_DUEL_STATS["bold"])
        loadout = self._get_loadout(discord_id, guild_id)
        stats = self._apply_gear_to_combat(base_stats, loadout)
        # Pinnacle relics fold their combat-side rolls in here as well.
        stats = self._apply_pinnacle_relic_stats(stats, loadout)

        cheers = self._get_cheers(tunnel)
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        # Pinnacle phases inherit a small accuracy penalty in higher phases
        # so the late-fight feels meaningful even before BOSS_PHASE3 kicks in.
        phase_penalty = 0.0
        if phase_idx == 2:
            phase_penalty = 0.10
        elif phase_idx == 3:
            phase_penalty = 0.15

        # Pinnacle is the Tier 8 fight — use its archetype per phase, not the
        # per-boss BOSS_ARCHETYPE_BY_ID lookup.
        scaled = self._scale_boss_stats(
            stats,
            boss_id=pinnacle_id,
            at_boss=PINNACLE_DEPTH,
            prestige_level=prestige_level,
            echo_applied=False,
            archetype_name=phase_def.archetype,
        )
        fresh_boss_hp = int(scaled["boss_hp"])
        # Look up any pending phase event left over from the previous
        # phase transition (one-shot, consumed at end of this fight).
        pin_entry_now = self._read_boss_progress_entry(boss_progress, PINNACLE_DEPTH)
        pending_event_id = pin_entry_now.get("pending_phase_event_id")
        phase_event_obj = None
        if pending_event_id:
            phase_event_obj = next(
                (e for e in PHASE_TRANSITION_EVENTS if e.id == pending_event_id),
                None,
            )
        if phase_event_obj is not None:
            fresh_boss_hp = max(1, fresh_boss_hp + int(phase_event_obj.boss_hp_delta))

        # Carry over persisted HP within the SAME phase (mid-phase retreat
        # leaves the boss wounded). Phase transitions reset HP because each
        # phase is a new fight. Use a phase-suffixed key in boss_progress
        # so we don't conflate phase 1 HP with phase 2 HP.
        phase_key = f"{PINNACLE_DEPTH}:{phase_idx}"
        boss_hp, boss_hp_max = self._resolve_persisted_boss_hp(
            boss_progress, phase_key, fresh_boss_hp, now,
        )
        boss_hit_chance = float(scaled["boss_hit"])
        boss_dmg = int(scaled["boss_dmg"])

        # Luminosity penalty.
        lum_value = self._get_luminosity(tunnel)
        lum_hit_offset, lum_dmg_bonus = _luminosity_combat_penalty(lum_value)
        boss_dmg += lum_dmg_bonus

        # Phase event round-by-round offsets/deltas.
        if phase_event_obj is not None:
            boss_hit_chance = max(
                0.05, min(0.95, boss_hit_chance + float(phase_event_obj.boss_hit_offset)),
            )
            boss_dmg = max(1, boss_dmg + int(phase_event_obj.boss_dmg_delta))

        # Player hit calc — pinnacle uses tier+prestige penalty from
        # the lookup tables, plus an inter-phase penalty.
        depth_hit_penalty = BOSS_TIER_BONUS[PINNACLE_DEPTH]["pen"]
        prestige_hit_penalty = BOSS_PRESTIGE_BONUS.get(
            prestige_level, BOSS_PRESTIGE_BONUS[max(BOSS_PRESTIGE_BONUS)],
        )["pen"]
        player_hit = (
            stats["player_hit"]
            - depth_hit_penalty - prestige_hit_penalty - phase_penalty
            + cheer_bonus
            + lum_hit_offset
        )
        if phase_event_obj is not None:
            player_hit += float(phase_event_obj.player_hit_offset)
        if wager == 0:
            player_hit *= BOSS_FREE_FIGHT_ACCURACY_MOD
        player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))

        player_hp = int(stats["player_hp"])
        if phase_event_obj is not None:
            player_hp = max(1, player_hp + int(phase_event_obj.player_hp_delta))
        player_dmg = int(stats["player_dmg"])
        if phase_event_obj is not None:
            player_dmg = max(0, player_dmg + int(phase_event_obj.player_dmg_delta))
        # Pinnacle relic stat: dmg per 100 depth (300 → +3 per stack).
        player_dmg += self._pinnacle_dmg_per_100_count(loadout) * (PINNACLE_DEPTH // 100)

        # Consume the pending phase event (one-shot) so it doesn't fire again.
        if phase_event_obj is not None:
            pin_entry_now.pop("pending_phase_event_id", None)
            boss_progress[str(PINNACLE_DEPTH)] = pin_entry_now

        win_chance = _approx_duel_win_prob(
            player_hp=player_hp,
            boss_hp=boss_hp,
            player_hit=player_hit,
            player_dmg=player_dmg,
            boss_hit=boss_hit_chance,
            boss_dmg=boss_dmg,
        )

        # Roll a mid-fight mechanic from this phase's pool. Pinnacle phase
        # mechanics are stronger and more bespoke than tier-boss mechanics
        # (see services.dig_constants.PINNACLE_BOSSES[*].phases[*].mechanic_pool).
        from domain.models.boss_mechanics import (
            get_mechanic as _get_mechanic,
        )
        mechanic_id = ""
        if phase_def.mechanic_pool:
            mechanic_id = random.Random().choice(list(phase_def.mechanic_pool))
        mechanic = _get_mechanic(mechanic_id) if mechanic_id else None
        attempts = (tunnel.get("boss_attempts", 0) or 0) + 1

        # If the rolled mechanic is a timed-input prompt (arithmetic / riddle),
        # generate the challenge now so it persists with the duel state.
        challenge: dict | None = None
        if mechanic is not None and mechanic.timed_input_kind:
            challenge = self._generate_timed_challenge(mechanic.timed_input_kind)

        round_log: list[dict] = []
        won: bool | None = None
        for round_num in range(1, BOSS_ROUND_CAP + 1):
            # If a mechanic is scheduled for THIS round, pause and persist.
            if (mechanic is not None
                    and round_num == mechanic.trigger_round
                    and player_hp > 0 and boss_hp > 0):
                # Pinnacle pauses use the same dig_active_duels table as
                # regular boss duels. The pinnacle is identified by storing
                # the pinnacle_id in boss_id (since pinnacle ids are
                # disjoint from BOSSES_BY_ID), with extra context in
                # status_effects under "pinnacle_state".
                state = {
                    "boss_id": pinnacle_id,
                    "tier": PINNACLE_DEPTH,
                    "mechanic_id": mechanic_id,
                    "risk_tier": risk_tier,
                    "wager": wager,
                    "player_hp": player_hp,
                    "boss_hp": boss_hp,
                    "round_num": round_num,
                    "round_log": json.dumps(round_log),
                    "pending_prompt": json.dumps(
                        self._serialize_prompt(mechanic)
                    ),
                    "rng_state": "",
                    "status_effects": json.dumps({
                        "attempts_this_fight": attempts,
                        "initial_win_chance": win_chance,
                        "pinnacle_state": {
                            "phase": phase_idx,
                            "boss_hp_max": boss_hp_max,
                            "phase_key": phase_key,
                        },
                        "timed_challenge": challenge,
                        "gear_snapshot_ids": [
                            int(p.id)
                            for p in (loadout.weapon, loadout.armor, loadout.boots)
                            if p is not None
                        ],
                    }),
                    "echo_applied": 0,
                    "echo_killer_id": None,
                    "player_hit": player_hit,
                    "player_dmg": player_dmg,
                    "boss_hit": boss_hit_chance,
                    "boss_dmg": boss_dmg,
                }
                self.dig_repo.save_active_duel(discord_id, guild_id, state)
                response_extras = {}
                if challenge is not None:
                    response_extras["timed_challenge"] = {
                        "kind": challenge["kind"],
                        "question": challenge["question"],
                        "time_window_seconds": challenge["time_window_seconds"],
                        "started_at": challenge["started_at"],
                    }
                return self._ok(
                    pending_prompt=self._serialize_prompt(mechanic),
                    boss_id=pinnacle_id,
                    boss_name=phase_def.title,
                    mechanic_id=mechanic_id,
                    boundary=PINNACLE_DEPTH,
                    risk_tier=risk_tier,
                    wager=wager,
                    player_hp=player_hp,
                    boss_hp=boss_hp,
                    round_num=round_num,
                    round_log=round_log,
                    win_chance=round(win_chance, 2),
                    is_pinnacle=True,
                    phase=phase_idx,
                    phase_total=3,
                    luminosity_display=self._luminosity_combat_display(tunnel),
                    **response_extras,
                )

            entry: dict = {"round": round_num}
            if random.random() < player_hit:
                boss_hp -= player_dmg
            entry["boss_hp"] = max(0, boss_hp)
            if boss_hp <= 0:
                won = True
                round_log.append(entry)
                break
            if random.random() < boss_hit_chance:
                player_hp -= boss_dmg
            entry["player_hp"] = max(0, player_hp)
            round_log.append(entry)
            if player_hp <= 0:
                won = False
                break
        else:
            won = False

        # Tick gear durability.
        broken_ids = self.dig_repo.tick_gear_durability(discord_id, guild_id)
        gear_broken_names: list[str] = []
        if broken_ids:
            pre_loadout = self._get_loadout(discord_id, guild_id)
            name_by_id = {
                p.id: p.tier_def.name
                for p in (pre_loadout.weapon, pre_loadout.armor, pre_loadout.boots)
                if p is not None
            }
            gear_broken_names = [name_by_id.get(i, "a piece of gear") for i in broken_ids]

        return self._finalize_pinnacle_outcome(
            discord_id=discord_id, guild_id=guild_id, tunnel=tunnel,
            pinnacle_id=pinnacle_id, pinnacle=pinnacle, phase_def=phase_def,
            phase_idx=phase_idx, phase_key=phase_key,
            boss_progress=boss_progress, won=won,
            boss_hp=boss_hp, boss_hp_max=boss_hp_max,
            risk_tier=risk_tier, wager=wager,
            win_chance=win_chance, attempts=attempts,
            round_log=round_log,
            gear_broken_names=gear_broken_names,
            prestige_level=prestige_level, depth=depth, now=now,
        )

    def _resume_pinnacle_duel(
        self,
        discord_id: int,
        guild_id,
        option_idx: int,
        state_row: dict,
    ) -> dict:
        """Resume a paused pinnacle duel after the player picks an option.

        Mirrors ``resume_boss_duel`` for regular bosses, but routes the
        post-resolution branches through the pinnacle's 3-phase / relic
        drop / prestige-gate logic in ``_fight_pinnacle``'s tail.
        """
        from domain.models.boss_mechanics import (
            get_mechanic as _get_mechanic,
        )

        mechanic = _get_mechanic(state_row["mechanic_id"])
        if mechanic is None:
            self.dig_repo.clear_active_duel(discord_id, guild_id)
            return self._error("Pinnacle duel references an unknown mechanic; cleared.")

        try:
            status_effects = json.loads(state_row["status_effects"] or "{}")
        except (json.JSONDecodeError, TypeError):
            status_effects = {}
        try:
            round_log = json.loads(state_row["round_log"] or "[]")
        except (json.JSONDecodeError, TypeError):
            round_log = []

        if not 0 <= option_idx < len(mechanic.options):
            option_idx = mechanic.safe_option_idx
        option = mechanic.options[option_idx]

        player_hp = int(state_row["player_hp"])
        boss_hp = int(state_row["boss_hp"])
        round_num = int(state_row["round_num"])

        narrative, player_hp, boss_hp, status_effects = (
            self._apply_option_outcome_to_state(
                option=option,
                player_hp=player_hp,
                boss_hp=boss_hp,
                status_effects=status_effects,
            )
        )
        round_log.append({
            "round": round_num,
            "mechanic_id": state_row["mechanic_id"],
            "option_idx": option_idx,
            "option_label": option.label,
            "narrative": narrative,
            "player_hp": max(0, player_hp),
            "boss_hp": max(0, boss_hp),
        })

        won: bool | None = None
        if boss_hp <= 0:
            won = True
        elif player_hp <= 0:
            won = False

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            self.dig_repo.clear_active_duel(discord_id, guild_id)
            return self._error("Tunnel disappeared during pinnacle duel.")
        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id

        player_hit = float(state_row["player_hit"])
        player_dmg = int(state_row["player_dmg"])
        boss_hit_chance = float(state_row["boss_hit"])
        boss_dmg = int(state_row["boss_dmg"])

        # Continue remaining auto-rounds if option didn't decide it.
        if won is None:
            for r in range(round_num + 1, BOSS_ROUND_CAP + 1):
                entry: dict = {"round": r}
                if random.random() < player_hit:
                    boss_hp -= player_dmg
                entry["boss_hp"] = max(0, boss_hp)
                if boss_hp <= 0:
                    won = True
                    round_log.append(entry)
                    break
                if random.random() < boss_hit_chance:
                    player_hp -= boss_dmg
                entry["player_hp"] = max(0, player_hp)
                round_log.append(entry)
                if player_hp <= 0:
                    won = False
                    break
            else:
                won = False

        # Pinnacle state restored from status_effects; falls back to tunnel
        # values when missing (e.g. legacy state row).
        pinnacle_state = status_effects.get("pinnacle_state") or {}
        phase_idx = int(pinnacle_state.get("phase") or tunnel.get("pinnacle_phase") or 1)
        boss_hp_max = int(pinnacle_state.get("boss_hp_max") or boss_hp or 1)
        phase_key = pinnacle_state.get("phase_key") or f"{PINNACLE_DEPTH}:{phase_idx}"

        # Tick durability for the gear that fought this fight.
        gear_snapshot_ids = status_effects.get("gear_snapshot_ids") or []
        gear_broken_names: list[str] = []
        if gear_snapshot_ids:
            name_by_id: dict[int, str] = {}
            for gid in gear_snapshot_ids:
                row = self.dig_repo.get_gear_by_id(int(gid))
                if row is None:
                    continue
                piece = self._hydrate_gear_piece(row)
                if piece is not None:
                    name_by_id[piece.id] = piece.tier_def.name
            broken_ids = self.dig_repo.tick_gear_durability_ids(
                [int(g) for g in gear_snapshot_ids]
            )
            gear_broken_names = [name_by_id.get(i, "a piece of gear") for i in broken_ids]
        else:
            broken_ids = self.dig_repo.tick_gear_durability(discord_id, guild_id)
            if broken_ids:
                pre_loadout = self._get_loadout(discord_id, guild_id)
                name_by_id = {
                    p.id: p.tier_def.name
                    for p in (pre_loadout.weapon, pre_loadout.armor, pre_loadout.boots)
                    if p is not None
                }
                gear_broken_names = [name_by_id.get(i, "a piece of gear") for i in broken_ids]

        # Clear the paused state row before returning.
        self.dig_repo.clear_active_duel(discord_id, guild_id)

        win_chance = float(status_effects.get("initial_win_chance") or 0.5)
        attempts = int(status_effects.get("attempts_this_fight") or 1)
        risk_tier = state_row["risk_tier"]
        wager = int(state_row["wager"])
        boss_progress = self._get_boss_progress(tunnel)
        pinnacle_id = state_row["boss_id"]
        pinnacle = PINNACLE_BOSSES.get(pinnacle_id)
        if pinnacle is None:
            return self._error("Pinnacle reference disappeared.")
        phase_def = pinnacle.phases[phase_idx - 1]
        prestige_level = tunnel.get("prestige_level", 0) or 0
        depth = tunnel.get("depth", 0)
        now = int(time.time())

        return self._finalize_pinnacle_outcome(
            discord_id=discord_id, guild_id=guild_id, tunnel=tunnel,
            pinnacle_id=pinnacle_id, pinnacle=pinnacle, phase_def=phase_def,
            phase_idx=phase_idx, phase_key=phase_key,
            boss_progress=boss_progress, won=won,
            boss_hp=boss_hp, boss_hp_max=boss_hp_max,
            risk_tier=risk_tier, wager=wager,
            win_chance=win_chance, attempts=attempts,
            round_log=round_log,
            gear_broken_names=gear_broken_names,
            prestige_level=prestige_level, depth=depth, now=now,
        )

    def _finalize_pinnacle_outcome(
        self,
        *,
        discord_id: int,
        guild_id,
        tunnel: dict,
        pinnacle_id: str,
        pinnacle,
        phase_def,
        phase_idx: int,
        phase_key: str,
        boss_progress: dict,
        won: bool,
        boss_hp: int,
        boss_hp_max: int,
        risk_tier: str,
        wager: int,
        win_chance: float,
        attempts: int,
        round_log: list,
        gear_broken_names: list,
        prestige_level: int,
        depth: int,
        now: int,
    ) -> dict:
        """Shared end-of-pinnacle-fight resolution used by both
        ``_fight_pinnacle`` and ``_resume_pinnacle_duel``."""
        boss_name = phase_def.title

        if won:
            if phase_idx < 3:
                phase_event = random.choice(PHASE_TRANSITION_EVENTS)
                next_phase = phase_idx + 1
                boss_progress.pop(phase_key, None)
                pin_entry = self._read_boss_progress_entry(boss_progress, PINNACLE_DEPTH)
                pin_entry["status"] = (
                    "phase1_defeated" if phase_idx == 1 else "phase2_defeated"
                )
                pin_entry["last_outcome"] = "defeated"
                pin_entry["first_meet_seen"] = True
                pin_entry["boss_id"] = pinnacle_id
                # Stash the event id so the next phase's fight can apply its
                # round-by-round offsets (hit/dmg). Pre-fight effects (HP and
                # luminosity deltas) are applied right now.
                pin_entry["pending_phase_event_id"] = phase_event.id
                boss_progress[str(PINNACLE_DEPTH)] = pin_entry

                # Apply pre-fight effects of the event:
                # - luminosity_delta: clamp to [0, MAX] on tunnel
                # - boss_hp_delta: pre-seed the next phase's HP entry so the
                #   first boss_hp resolution starts wounded (or refreshed).
                lum_after = self._get_luminosity(tunnel)
                if phase_event.luminosity_delta:
                    lum_after = max(0, min(LUMINOSITY_MAX, lum_after + phase_event.luminosity_delta))
                next_phase_key = f"{PINNACLE_DEPTH}:{next_phase}"
                if phase_event.boss_hp_delta:
                    # Pre-seed wounded HP using a synthetic prior-engagement.
                    boss_progress[next_phase_key] = {
                        "boss_id": pinnacle_id,
                        "hp_remaining_delta": int(phase_event.boss_hp_delta),
                    }

                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    boss_progress=json.dumps(boss_progress),
                    pinnacle_phase=next_phase,
                    boss_attempts=attempts,
                    last_dig_at=now,
                    luminosity=lum_after,
                    last_lum_update_at=now,
                )
                next_title = pinnacle.phases[next_phase - 1].title
                transition_lines = pinnacle.phases[next_phase - 1].transition_dialogue
                transition = (
                    random.choice(transition_lines)
                    if transition_lines
                    else f"The {pinnacle.name} reshapes."
                )
                self.dig_repo.log_action(
                    discord_id=discord_id, guild_id=guild_id,
                    action_type="pinnacle_fight",
                    details=json.dumps({
                        "pinnacle_id": pinnacle_id,
                        "phase": phase_idx, "won": True,
                        "rounds": round_log,
                    }),
                )
                return self._ok(
                    won=True,
                    phase=phase_idx,
                    phase2_incoming=(next_phase == 2),
                    phase3_incoming=(next_phase == 3),
                    boss_name=boss_name,
                    boundary=PINNACLE_DEPTH,
                    risk_tier=risk_tier,
                    win_chance=round(win_chance, 2),
                    jc_delta=0,
                    payout=0,
                    new_depth=depth,
                    dialogue=transition,
                    next_phase_title=next_title,
                    phase_event_flavor=phase_event.flavor,
                    phase_event_description=phase_event.description,
                    achievements=[],
                    round_log=round_log,
                    is_pinnacle=True,
                    gear_broken=gear_broken_names,
                    gear_drop=None,
                    luminosity_display=self._luminosity_combat_display(tunnel),
                )

            # Phase 3 win — pinnacle defeated.
            new_depth = PINNACLE_DEPTH
            jc_reward = PINNACLE_BASE_JC_REWARD + PINNACLE_JC_PER_PRESTIGE * prestige_level
            relic_drop = self._drop_pinnacle_relic(discord_id, guild_id, tunnel, pinnacle_id)
            boss_progress.pop(phase_key, None)
            boss_progress[str(PINNACLE_DEPTH)] = {
                "status": "defeated",
                "last_outcome": "close_win" if win_chance < 0.6 else "defeated",
                "first_meet_seen": True,
                "boss_id": pinnacle_id,
                "hp_remaining": 0,
                "hp_max": boss_hp_max,
                "last_engaged_at": int(now),
            }
            prev_max_depth = tunnel.get("max_depth", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                max_depth=max(prev_max_depth, new_depth),
                boss_progress=json.dumps(boss_progress),
                boss_attempts=0,
                cheer_data=None,
                last_dig_at=now,
                pinnacle_phase=0,
            )
            self.player_repo.add_balance(discord_id, guild_id, jc_reward)
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="pinnacle_fight",
                details=json.dumps({
                    "pinnacle_id": pinnacle_id,
                    "phase": 3, "won": True,
                    "jc_delta": jc_reward,
                    "relic_id": relic_drop["artifact_id"],
                }),
            )
            return self._ok(
                won=True,
                phase=3,
                boss_name=pinnacle.name,
                boundary=PINNACLE_DEPTH,
                risk_tier=risk_tier,
                win_chance=round(win_chance, 2),
                jc_delta=jc_reward,
                payout=jc_reward,
                new_depth=new_depth,
                dialogue=f"You stand over the broken form of {pinnacle.name}.",
                pinnacle_relic=relic_drop,
                achievements=[],
                round_log=round_log,
                is_pinnacle=True,
                pinnacle_defeated=True,
                gear_broken=gear_broken_names,
                gear_drop=None,
                luminosity_display=self._luminosity_combat_display(tunnel),
            )

        # Loss
        knockback = random.randint(8, 16)
        new_depth = max(0, depth - knockback)
        jc_delta = -wager if wager > 0 else 0
        self._persist_boss_hp_after_fight(
            boss_progress, phase_key, pinnacle_id,
            ending_hp=max(0, boss_hp), hp_max=boss_hp_max,
            won=False, outcome="loss", now=now,
        )
        pin_entry = self._read_boss_progress_entry(boss_progress, PINNACLE_DEPTH)
        pin_entry["last_outcome"] = "loss"
        pin_entry["first_meet_seen"] = True
        pin_entry["boss_id"] = pinnacle_id
        boss_progress[str(PINNACLE_DEPTH)] = pin_entry

        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=new_depth,
            boss_progress=json.dumps(boss_progress),
            boss_attempts=attempts,
            cheer_data=None,
            last_dig_at=now,
        )
        if wager > 0:
            self.player_repo.add_balance(discord_id, guild_id, -wager)
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="pinnacle_fight",
            details=json.dumps({
                "pinnacle_id": pinnacle_id,
                "phase": phase_idx, "won": False,
                "rounds": round_log,
                "boss_hp_remaining": max(0, boss_hp),
            }),
        )
        return self._ok(
            won=False,
            phase=phase_idx,
            boss_name=boss_name,
            boundary=PINNACLE_DEPTH,
            risk_tier=risk_tier,
            win_chance=round(win_chance, 2),
            jc_delta=jc_delta,
            knockback=knockback,
            new_depth=new_depth,
            boss_hp_remaining=max(0, boss_hp),
            boss_hp_max=boss_hp_max,
            dialogue=f"{boss_name} sends you reeling back {knockback} blocks!",
            achievements=[],
            round_log=round_log,
            is_pinnacle=True,
            gear_broken=gear_broken_names,
            gear_drop=None,
            luminosity_display=self._luminosity_combat_display(tunnel),
        )

    # =====================================================================
    # Multi-boss tier state machine — reactive mid-fight prompts
    # =====================================================================
    # ``start_boss_duel`` is the entry point for the new mid-fight-prompt
    # flow. It does everything ``fight_boss`` does up to the auto-round loop,
    # but if the boss's rolled mechanic (drawn from ``BossDef.mechanic_pool``)
    # is scheduled to trigger this fight, it pauses at the trigger round,
    # persists duel state to ``dig_active_duels``, and returns a
    # ``pending_prompt`` for the UI to render.
    #
    # ``resume_boss_duel`` is called when the player clicks one of the three
    # reactive option buttons. It loads the paused state, rolls the option's
    # outcome distribution, applies the result to the duel, continues the
    # auto-rounds to final resolution, and clears the paused state row.
    #
    # The legacy ``fight_boss`` entry point remains synchronous and does NOT
    # trigger mid-fight prompts — it's used by tests and by any caller that
    # wants a one-shot resolution. The new UI paths use ``start_boss_duel``
    # and ``resume_boss_duel``.
    # =====================================================================

    def start_boss_duel(
        self, discord_id: int, guild_id, risk_tier: str, wager: int = 0,
    ) -> dict:
        """Start a boss duel. Pauses at the rolled mechanic's trigger round."""
        from domain.models.boss_mechanics import (
            get_mechanic as _get_mechanic,
        )

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")
        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id

        # Abandoned-duel cleanup: if a previous mid-fight pause was never
        # resumed, the stale dig_active_duels row would otherwise leak the
        # durability tick for that fight. Tick once for the prior fight
        # and clear the row before starting a fresh duel.
        stale = self.dig_repo.get_active_duel(discord_id, guild_id)
        if stale is not None:
            self.dig_repo.tick_gear_durability(discord_id, guild_id)
            self.dig_repo.clear_active_duel(discord_id, guild_id)

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

        # Pinnacle uses its own resolver — no mid-fight prompts (yet).
        if self._is_pinnacle_depth(at_boss):
            return self._fight_pinnacle(discord_id, guild_id, tunnel, risk_tier, wager)

        # Ensure a specific boss is locked for this tunnel at this tier.
        boss = self._ensure_boss_locked(discord_id, guild_id, tunnel, at_boss)

        # Pick which mechanic fires this fight (variance on what prompt fires).
        mechanic_id = ""
        if boss.mechanic_pool:
            mechanic_id = random.Random().choice(list(boss.mechanic_pool))
        mechanic = _get_mechanic(mechanic_id) if mechanic_id else None

        # Stats build — mirrors fight_boss flow with gear modifiers folded in.
        base_stats = BOSS_DUEL_STATS.get(risk_tier, BOSS_DUEL_STATS["bold"])
        loadout = self._get_loadout(discord_id, guild_id)
        stats = self._apply_gear_to_combat(base_stats, loadout)
        tier_index = {"cautious": 0, "bold": 1, "reckless": 2}.get(risk_tier, 1)
        payouts = BOSS_PAYOUTS.get(at_boss, (2.0, 3.0, 6.0))
        multiplier = payouts[tier_index] if tier_index < len(payouts) else 2.0

        prestige_level = tunnel.get("prestige_level", 0) or 0
        cheers = self._get_cheers(tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        phase2_penalty = 0.0
        _phase_status = boss_progress.get(str(at_boss))
        if _phase_status == "phase1_defeated" and at_boss in BOSS_PHASE2:
            phase2_penalty = abs(BOSS_PHASE2[at_boss].win_odds_penalty)
        elif _phase_status == "phase2_defeated" and at_boss in BOSS_PHASE3:
            phase2_penalty = abs(BOSS_PHASE3[at_boss].win_odds_penalty)

        active_echo = self.dig_repo.get_active_boss_echo(guild_id, boss.boss_id)
        echo_applied = bool(
            active_echo
            and active_echo.get("killer_discord_id") != discord_id
        )

        scaled = self._scale_boss_stats(
            stats,
            boss_id=boss.boss_id,
            at_boss=at_boss,
            prestige_level=prestige_level,
            echo_applied=echo_applied,
        )
        fresh_boss_hp = int(scaled["boss_hp"])
        # Carry persisted HP from prior unfinished engagements (with regen).
        boss_hp, boss_hp_max = self._resolve_persisted_boss_hp(
            boss_progress, at_boss, fresh_boss_hp, int(time.time()),
        )
        boss_hit_chance = float(scaled["boss_hit"])
        boss_dmg = int(scaled["boss_dmg"])

        # Luminosity combat penalty.
        lum_value = self._get_luminosity(tunnel)
        lum_hit_offset, lum_dmg_bonus = _luminosity_combat_penalty(lum_value)
        boss_dmg += lum_dmg_bonus

        _tk = at_boss if at_boss in BOSS_TIER_BONUS else max((k for k in BOSS_TIER_BONUS if k <= at_boss), default=25)
        depth_hit_penalty = BOSS_TIER_BONUS[_tk]["pen"]
        prestige_hit_penalty = BOSS_PRESTIGE_BONUS.get(prestige_level, BOSS_PRESTIGE_BONUS[max(BOSS_PRESTIGE_BONUS)])["pen"]
        player_hit = (
            stats["player_hit"]
            - depth_hit_penalty - prestige_hit_penalty - phase2_penalty
            + cheer_bonus
            + lum_hit_offset
        )
        if wager == 0:
            player_hit *= BOSS_FREE_FIGHT_ACCURACY_MOD
        player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))

        player_hp = int(stats["player_hp"])
        player_dmg = int(stats["player_dmg"])

        win_chance = _approx_duel_win_prob(
            player_hp=player_hp,
            boss_hp=boss_hp,
            player_hit=player_hit,
            player_dmg=player_dmg,
            boss_hit=boss_hit_chance,
            boss_dmg=boss_dmg,
        )
        attempts = (tunnel.get("boss_attempts", 0) or 0) + 1

        # Run auto-rounds until trigger or resolution.
        round_log: list[dict] = []
        status_effects: dict = {}
        won: bool | None = None
        for round_num in range(1, BOSS_ROUND_CAP + 1):
            # If a mechanic is scheduled for THIS round, pause and persist.
            if (mechanic is not None
                    and round_num == mechanic.trigger_round
                    and player_hp > 0 and boss_hp > 0):
                state = {
                    "boss_id": boss.boss_id,
                    "tier": at_boss,
                    "mechanic_id": mechanic_id,
                    "risk_tier": risk_tier,
                    "wager": wager,
                    "player_hp": player_hp,
                    "boss_hp": boss_hp,
                    "round_num": round_num,
                    "round_log": json.dumps(round_log),
                    "pending_prompt": json.dumps(
                        self._serialize_prompt(mechanic)
                    ),
                    "rng_state": "",
                    "status_effects": json.dumps({
                        **status_effects,
                        "attempts_this_fight": attempts,
                        "initial_win_chance": win_chance,
                        "multiplier": multiplier,
                        # Snapshot the gear ids that fought THIS fight so the
                        # durability tick on resume hits these pieces, even
                        # if the player swapped gear during the pause.
                        "gear_snapshot_ids": [
                            int(p.id)
                            for p in (loadout.weapon, loadout.armor, loadout.boots)
                            if p is not None
                        ],
                    }),
                    "echo_applied": 1 if echo_applied else 0,
                    "echo_killer_id": (
                        active_echo.get("killer_discord_id")
                        if echo_applied and active_echo else None
                    ),
                    "player_hit": player_hit,
                    "player_dmg": player_dmg,
                    "boss_hit": boss_hit_chance,
                    "boss_dmg": boss_dmg,
                }
                self.dig_repo.save_active_duel(discord_id, guild_id, state)
                return self._ok(
                    pending_prompt=self._serialize_prompt(mechanic),
                    boss_id=boss.boss_id,
                    boss_name=boss.name,
                    mechanic_id=mechanic_id,
                    boundary=at_boss,
                    risk_tier=risk_tier,
                    wager=wager,
                    player_hp=player_hp,
                    boss_hp=boss_hp,
                    round_num=round_num,
                    round_log=round_log,
                    win_chance=round(win_chance, 2),
                    echo_applied=echo_applied,
                    echo_killer_id=(
                        active_echo.get("killer_discord_id")
                        if echo_applied and active_echo else None
                    ),
                    luminosity_display=self._luminosity_combat_display(tunnel),
                )

            entry, player_hp, boss_hp, terminal = self._run_one_round(
                round_num=round_num,
                player_hp=player_hp, boss_hp=boss_hp,
                player_hit=player_hit, player_dmg=player_dmg,
                boss_hit=boss_hit_chance, boss_dmg=boss_dmg,
                status_effects=status_effects,
            )
            round_log.append(entry)
            if terminal is True:
                won = True
                break
            if terminal is False:
                won = False
                break
        if won is None:
            # Round cap hit.
            won = False

        # Auto-resolve without a prompt firing.
        return self._resolve_duel_outcome(
            discord_id=discord_id, guild_id=guild_id,
            tunnel=tunnel, boss=boss, at_boss=at_boss,
            risk_tier=risk_tier, wager=wager,
            won=won, round_log=round_log,
            echo_applied=echo_applied, active_echo=active_echo,
            win_chance=win_chance,
            multiplier=multiplier, prestige_level=prestige_level,
            attempts=attempts, boss_progress=dict(boss_progress),
            depth=depth,
            ending_boss_hp=int(boss_hp), boss_hp_max=int(boss_hp_max),
        )

    def resume_boss_duel(
        self, discord_id: int, guild_id, option_idx: int,
        *, state_row: dict | None = None,
    ) -> dict:
        """Resume a paused duel after the player picks a reactive option.

        ``state_row`` may be supplied by the caller when the row was already
        atomically claimed (see ``submit_timed_answer``). When omitted, the
        row is read from the repo as usual.
        """
        from domain.models.boss_mechanics import get_mechanic as _get_mechanic
        from services.dig_constants import get_boss_by_id as _get_boss

        if state_row is None:
            state_row = self.dig_repo.get_active_duel(discord_id, guild_id)
        if state_row is None:
            return self._error("No active duel to resume.")

        # Pinnacle pauses store the pinnacle_id in state_row["boss_id"];
        # route them to the dedicated resolver so the post-fight branch
        # respects 3-phase + relic-drop rules.
        if state_row["boss_id"] in PINNACLE_BOSSES:
            return self._resume_pinnacle_duel(
                discord_id, guild_id, option_idx, state_row,
            )

        boss = _get_boss(state_row["boss_id"])
        if boss is None:
            self.dig_repo.clear_active_duel(discord_id, guild_id)
            return self._error("Duel references an unknown boss; cleared.")

        mechanic = _get_mechanic(state_row["mechanic_id"])
        if mechanic is None:
            self.dig_repo.clear_active_duel(discord_id, guild_id)
            return self._error("Duel references an unknown mechanic; cleared.")

        try:
            status_effects = json.loads(state_row["status_effects"] or "{}")
        except (json.JSONDecodeError, TypeError):
            status_effects = {}
        try:
            round_log = json.loads(state_row["round_log"] or "[]")
        except (json.JSONDecodeError, TypeError):
            round_log = []

        if not 0 <= option_idx < len(mechanic.options):
            option_idx = mechanic.safe_option_idx
        option = mechanic.options[option_idx]

        # Roll the option's distribution and apply deltas.
        player_hp = int(state_row["player_hp"])
        boss_hp = int(state_row["boss_hp"])
        round_num = int(state_row["round_num"])

        narrative, player_hp, boss_hp, status_effects = (
            self._apply_option_outcome_to_state(
                option=option,
                player_hp=player_hp,
                boss_hp=boss_hp,
                status_effects=status_effects,
            )
        )
        round_log.append({
            "round": round_num,
            "mechanic_id": state_row["mechanic_id"],
            "option_idx": option_idx,
            "option_label": option.label,
            "narrative": narrative,
            "player_hp": max(0, player_hp),
            "boss_hp": max(0, boss_hp),
        })

        # Immediate HP check after option outcome.
        won: bool | None = None
        if boss_hp <= 0:
            won = True
        elif player_hp <= 0:
            won = False

        # Re-load tunnel for fresh state (caller may have dug, etc.).
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            self.dig_repo.clear_active_duel(discord_id, guild_id)
            return self._error("Tunnel disappeared during duel.")
        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id
        depth = tunnel.get("depth", 0)

        player_hit = float(state_row["player_hit"])
        player_dmg = int(state_row["player_dmg"])
        boss_hit = float(state_row["boss_hit"])
        boss_dmg = int(state_row["boss_dmg"])

        at_boss = int(state_row["tier"])

        # Continue remaining auto-rounds if duel hasn't resolved on the option.
        if won is None:
            for r in range(round_num + 1, BOSS_ROUND_CAP + 1):
                entry, player_hp, boss_hp, terminal = self._run_one_round(
                    round_num=r,
                    player_hp=player_hp, boss_hp=boss_hp,
                    player_hit=player_hit, player_dmg=player_dmg,
                    boss_hit=boss_hit, boss_dmg=boss_dmg,
                    status_effects=status_effects,
                )
                round_log.append(entry)
                if terminal is True:
                    won = True
                    break
                if terminal is False:
                    won = False
                    break
            if won is None:
                won = False  # round cap

        # Reconstruct active_echo-ish info for reporting.
        active_echo = None
        if int(state_row["echo_applied"] or 0):
            active_echo = {
                "killer_discord_id": state_row.get("echo_killer_id"),
            }
        echo_applied = bool(state_row["echo_applied"])

        multiplier = float(status_effects.get(
            "multiplier",
            BOSS_PAYOUTS.get(at_boss, (2.0, 3.0, 6.0))[
                {"cautious": 0, "bold": 1, "reckless": 2}.get(state_row["risk_tier"], 1)
            ],
        ))
        win_chance = float(status_effects.get("initial_win_chance", 0.0))
        attempts = int(
            status_effects.get("attempts_this_fight")
            or ((tunnel.get("boss_attempts", 0) or 0) + 1)
        )
        prestige_level = tunnel.get("prestige_level", 0) or 0
        boss_progress = self._get_boss_progress_entries(tunnel)

        self.dig_repo.clear_active_duel(discord_id, guild_id)

        snapshot_ids = status_effects.get("gear_snapshot_ids") or []
        # Reconstruct boss_hp_max from the round log (highest post-hit value
        # plus the player's per-round damage) to seed persisted-HP tracking.
        approx_hp_max = max(
            (int(r.get("boss_hp", 0)) for r in round_log if "boss_hp" in r),
            default=int(boss_hp),
        )
        if approx_hp_max < int(boss_hp):
            approx_hp_max = max(int(boss_hp), 1)
        approx_hp_max += int(state_row["player_dmg"])
        return self._resolve_duel_outcome(
            discord_id=discord_id, guild_id=guild_id,
            tunnel=tunnel, boss=boss, at_boss=at_boss,
            risk_tier=state_row["risk_tier"],
            wager=int(state_row["wager"]),
            won=won, round_log=round_log,
            echo_applied=echo_applied, active_echo=active_echo,
            win_chance=win_chance,
            multiplier=multiplier, prestige_level=prestige_level,
            attempts=attempts, boss_progress=boss_progress,
            depth=depth,
            gear_snapshot_ids=snapshot_ids,
            ending_boss_hp=int(boss_hp), boss_hp_max=int(approx_hp_max),
        )

    # --- helpers --------------------------------------------------------

    def _serialize_prompt(self, mechanic) -> dict:
        """Turn a BossMechanic into a JSON-safe dict for persistence / UI."""
        return {
            "mechanic_id": mechanic.id,
            "archetype": mechanic.archetype,
            "prompt_title": mechanic.prompt_title,
            "prompt_description": mechanic.prompt_description,
            "options": [
                {"option_idx": i, "label": opt.label}
                for i, opt in enumerate(mechanic.options)
            ],
            "safe_option_idx": mechanic.safe_option_idx,
        }

    def _run_one_round(
        self,
        *,
        round_num: int,
        player_hp: int, boss_hp: int,
        player_hit: float, player_dmg: int,
        boss_hit: float, boss_dmg: int,
        status_effects: dict,
    ) -> tuple[dict, int, int, bool | None]:
        """Run one auto-round. Returns (entry, player_hp, boss_hp, terminal).

        ``terminal`` is True if player won (boss at 0), False if lost
        (player at 0), None if neither. Mutates ``status_effects`` in-place
        to decrement DOTs and clear one-shot effects.
        """
        entry: dict = {"round": round_num}

        # Start-of-round effects
        if status_effects.get("boss_exposed_next_round"):
            boss_hp -= 1
            status_effects.pop("boss_exposed_next_round", None)
        burn = int(status_effects.get("burn_rounds_remaining", 0))
        if burn > 0:
            player_hp -= 1
            status_effects["burn_rounds_remaining"] = burn - 1
        bleed = int(status_effects.get("bleed_rounds_remaining", 0))
        if bleed > 0:
            player_hp -= 1
            status_effects["bleed_rounds_remaining"] = bleed - 1
        if boss_hp <= 0:
            entry["boss_hp"] = 0
            entry["player_hp"] = max(0, player_hp)
            return entry, player_hp, boss_hp, True
        if player_hp <= 0:
            entry["player_hp"] = 0
            entry["boss_hp"] = max(0, boss_hp)
            return entry, player_hp, boss_hp, False

        skip = status_effects.pop("skip_next_round_for", None)
        silenced = status_effects.pop("silenced_next_round", False)
        frost = status_effects.pop("frostbite_next_round", False)

        # Player swing
        if skip != "player":
            effective_player_hit = 0.0 if silenced else player_hit
            player_roll = random.random() < effective_player_hit
            if player_roll:
                boss_hp -= player_dmg
            entry["player_hit"] = player_roll
            entry["boss_hp"] = max(0, boss_hp)
        else:
            entry["player_hit"] = False
            entry["boss_hp"] = max(0, boss_hp)
            entry["skipped_player"] = True

        if boss_hp <= 0:
            return entry, player_hp, boss_hp, True

        # Boss swing
        if skip != "boss":
            boss_roll = random.random() < boss_hit
            if boss_roll:
                actual_dmg = boss_dmg + (1 if frost else 0)
                player_hp -= actual_dmg
            entry["boss_hit"] = boss_roll
            entry["player_hp"] = max(0, player_hp)
        else:
            entry["boss_hit"] = False
            entry["player_hp"] = max(0, player_hp)
            entry["skipped_boss"] = True

        if player_hp <= 0:
            return entry, player_hp, boss_hp, False

        return entry, player_hp, boss_hp, None

    def _apply_option_outcome_to_state(
        self, *, option, player_hp: int, boss_hp: int, status_effects: dict,
    ) -> tuple[str, int, int, dict]:
        """Roll the option's distribution, apply deltas, return (narrative, hp, hp, effects)."""
        from domain.models.boss_mechanics import EFFECT_APPLIERS as _EFFS

        roll_val = random.random()
        cum = 0.0
        chosen = option.outcome_rolls[-1]
        for o in option.outcome_rolls:
            cum += o.probability
            if roll_val < cum:
                chosen = o
                break

        new_status = dict(status_effects)
        player_hp += chosen.player_hp_delta
        boss_hp += chosen.boss_hp_delta
        if chosen.skip_next_round_for:
            new_status["skip_next_round_for"] = chosen.skip_next_round_for
        if chosen.status_effect and chosen.status_effect in _EFFS:
            # Appliers mutate a state-like dict in the same shape.
            fake_state = {"status_effects": new_status}
            _EFFS[chosen.status_effect](fake_state)
            new_status = fake_state.get("status_effects") or new_status
        return chosen.narrative, player_hp, boss_hp, new_status

    def _get_boss_progress_entries(self, tunnel: dict) -> dict:
        """Return the boss_progress JSON as {depth_str: entry_dict_or_str}."""
        raw = tunnel.get("boss_progress")
        if not raw:
            return {str(b): "active" for b in BOSS_BOUNDARIES}
        try:
            stored = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return {str(b): "active" for b in BOSS_BOUNDARIES}
        canonical = {str(b): "active" for b in BOSS_BOUNDARIES}
        canonical.update(stored)
        return canonical

    def _apply_stinger_on_loss(
        self, discord_id: int, guild_id, tunnel: dict, boss,
    ) -> tuple[int, int]:
        """Apply the boss's stinger effect. Returns (extra_knockback, extra_cooldown_s)."""
        from domain.models.boss_stingers import STINGER_REGISTRY as _STS

        stinger_id = getattr(boss, "stinger_id", "")
        if not stinger_id or stinger_id not in _STS:
            return 0, 0
        stinger = _STS[stinger_id]

        # Write cursed_status JSON onto the tunnel if present.
        if stinger.cursed_status:
            curse_raw = tunnel.get("stinger_curse")
            try:
                curse = json.loads(curse_raw) if curse_raw else {}
            except (json.JSONDecodeError, TypeError):
                curse = {}
            curse[stinger.cursed_status] = True
            curse["_boss_id"] = boss.boss_id
            self.dig_repo.update_tunnel(
                discord_id, guild_id, stinger_curse=json.dumps(curse),
            )
        return int(stinger.extra_knockback or 0), int(stinger.extended_cooldown_s or 0)

    def _resolve_duel_outcome(
        self, *, discord_id, guild_id, tunnel, boss, at_boss,
        risk_tier, wager, won, round_log, echo_applied, active_echo,
        win_chance, multiplier, prestige_level, attempts,
        boss_progress, depth,
        gear_snapshot_ids: list[int] | None = None,
        ending_boss_hp: int | None = None,
        boss_hp_max: int | None = None,
    ) -> dict:
        """Apply the win-branch or loss-branch post-processing and return the result dict.

        Mirrors ``fight_boss``'s win (lines 3613-3742) and loss (3743-3786)
        blocks; extended with per-boss stinger on loss.

        Defensively clears any ``dig_active_duels`` row regardless of which
        upstream path arrived here (``start_boss_duel`` auto-resolve,
        ``resume_boss_duel`` continuation, or a future admin/debug entry).
        The delete is idempotent so the auto-resolve path that never saved
        a row is cheap.
        """
        self.dig_repo.clear_active_duel(discord_id, guild_id)
        now = int(time.time())
        boss_name = boss.name if boss is not None else BOSS_NAMES.get(at_boss, "Unknown Boss")

        # Wear-and-tear: tick durability for the gear that actually fought
        # this fight. When resume_boss_duel forwards a ``gear_snapshot_ids``
        # list, those are the IDs that were equipped at start_boss_duel
        # time — use them so a player who swapped gear during the pause
        # doesn't burn durability on pieces they never wore. Auto-resolve
        # path (no snapshot) ticks the currently-equipped loadout.
        if gear_snapshot_ids:
            # Resolve names from the snapshot rows directly (those pieces
            # may no longer be equipped, so the loadout helper won't see
            # them).
            name_by_id: dict[int, str] = {}
            for gid in gear_snapshot_ids:
                row = self.dig_repo.get_gear_by_id(int(gid))
                if row is None:
                    continue
                piece = self._hydrate_gear_piece(row)
                if piece is not None:
                    name_by_id[piece.id] = piece.tier_def.name
            broken_ids = self.dig_repo.tick_gear_durability_ids(
                [int(g) for g in gear_snapshot_ids]
            )
        else:
            pre_tick_loadout = self._get_loadout(discord_id, guild_id)
            name_by_id = {}
            for piece in (pre_tick_loadout.weapon,
                          pre_tick_loadout.armor,
                          pre_tick_loadout.boots):
                if piece is not None:
                    name_by_id[piece.id] = piece.tier_def.name
            broken_ids = self.dig_repo.tick_gear_durability(discord_id, guild_id)
        gear_broken_names: list[str] = [
            name_by_id.get(i, "a piece of gear") for i in broken_ids
        ]

        ascension = self._get_ascension_effects(prestige_level)
        boss_payout_mult = 1.0 + ascension.get("boss_payout_multiplier", 0)

        if won:
            current_entry = boss_progress.get(str(at_boss), "active")
            current_status = (
                current_entry.get("status", "active")
                if isinstance(current_entry, dict)
                else current_entry
            )
            phase2_min_p = int(BOSS_PHASES.get("phase_2_min_prestige", 2))
            phase3_min_p = int(BOSS_PHASES.get("phase_3_min_prestige", 5))
            phase3_min_tier = int(BOSS_PHASES.get("phase_3_min_tier", 100))
            needs_phase2 = (
                prestige_level >= phase2_min_p
                and at_boss in BOSS_PHASE2
                and current_status == "active"
            )
            needs_phase3 = (
                prestige_level >= phase3_min_p
                and at_boss >= phase3_min_tier
                and at_boss in BOSS_PHASE3
                and current_status == "phase1_defeated"
            )

            if needs_phase2 or needs_phase3:
                next_status = "phase1_defeated" if needs_phase2 else "phase2_defeated"
                phase_def = BOSS_PHASE2[at_boss] if needs_phase2 else BOSS_PHASE3[at_boss]
                next_phase_num = 2 if needs_phase2 else 3
                phase_event = random.choice(PHASE_TRANSITION_EVENTS)
                # Mark next phase status, preserving boss_id when present.
                if isinstance(current_entry, dict):
                    current_entry["status"] = next_status
                    boss_progress[str(at_boss)] = current_entry
                else:
                    boss_progress[str(at_boss)] = {
                        "boss_id": boss.boss_id if boss else "",
                        "status": next_status,
                    }
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    boss_progress=json.dumps(boss_progress),
                    boss_attempts=attempts,
                    last_dig_at=now,
                )
                p_dialogue = phase_def.dialogue[min(attempts - 1, len(phase_def.dialogue) - 1)]
                self.dig_repo.log_action(
                    discord_id=discord_id, guild_id=guild_id,
                    action_type="boss_fight",
                    details=json.dumps({
                        "boundary": at_boss, "won": True, "risk": risk_tier,
                        "phase": next_phase_num - 1, "wager": wager, "rounds": round_log,
                    }),
                )
                return self._ok(
                    won=True,
                    phase=next_phase_num - 1,
                    phase2_incoming=needs_phase2,
                    phase3_incoming=needs_phase3,
                    boss_name=boss_name, boss_id=boss.boss_id if boss else "",
                    phase2_name=phase_def.name, phase2_title=phase_def.title,
                    phase_event_flavor=phase_event.flavor,
                    phase_event_description=phase_event.description,
                    boundary=at_boss, risk_tier=risk_tier,
                    win_chance=round(win_chance, 2),
                    jc_delta=0, payout=0,
                    new_depth=depth,
                    dialogue=p_dialogue,
                    achievements=[],
                    round_log=round_log,
                    echo_applied=echo_applied,
                    echo_killer_id=(
                        active_echo.get("killer_discord_id")
                        if echo_applied and active_echo else None
                    ),
                    gear_broken=gear_broken_names,
                    gear_drop=None,
                )

            # Full victory
            new_depth = at_boss
            echo_payout_mult = 0.7 if echo_applied else 1.0
            base_jc = int(wager * multiplier) if wager > 0 else random.randint(8, 18)
            # Honor drain_next_reward curse: -25% on this reward.
            curse_raw = tunnel.get("stinger_curse")
            drain_applied = False
            try:
                curse = json.loads(curse_raw) if curse_raw else {}
            except (json.JSONDecodeError, TypeError):
                curse = {}
            if curse.get("drain_next_reward"):
                base_jc = int(round(base_jc * 0.75))
                drain_applied = True
                curse.pop("drain_next_reward", None)
                # Persist cleared curse flag (keep other curses intact)
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    stinger_curse=(json.dumps(curse) if curse else None),
                )
            jc_delta = int(base_jc * boss_payout_mult * echo_payout_mult)

            # Mark defeated in the {boss_id, status} shape.
            existing_entry = boss_progress.get(str(at_boss))
            if isinstance(existing_entry, dict):
                existing_entry["status"] = "defeated"
                boss_progress[str(at_boss)] = existing_entry
            else:
                boss_progress[str(at_boss)] = {
                    "boss_id": boss.boss_id if boss else "",
                    "status": "defeated",
                }
            stat_point_awarded = self._award_boss_stat_point_if_first(
                discord_id, guild_id, tunnel, at_boss,
            )
            prev_max_depth = tunnel.get("max_depth", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                depth=new_depth,
                max_depth=max(prev_max_depth, new_depth),
                boss_progress=json.dumps(boss_progress),
                boss_attempts=0,
                cheer_data=None,
                last_dig_at=now,
            )
            self.dig_repo.record_boss_echo(
                guild_id=guild_id,
                boss_id=boss.boss_id if boss else "",
                depth=at_boss,
                killer_discord_id=discord_id,
                window_seconds=24 * 3600,
            )
            if wager > 0:
                self.player_repo.add_balance(
                    discord_id, guild_id,
                    int(wager * (multiplier * boss_payout_mult * echo_payout_mult - 1))
                    - (int(round(wager * multiplier * 0.25)) if drain_applied else 0),
                )
            else:
                self.player_repo.add_balance(discord_id, guild_id, jc_delta)

            achievements = self.check_achievements(
                discord_id, guild_id,
                {**tunnel, "depth": new_depth},
                {"action": "boss_win", "boundary": at_boss, "boss_progress": boss_progress},
            )
            dialogue_list = (
                boss.dialogue if (boss is not None and boss.dialogue)
                else BOSS_DIALOGUE.get(at_boss, ["..."])
            )
            defeat_msg = dialogue_list[-1] if dialogue_list else "Defeated!"
            # Boss-drop roll happens once per full kill, NOT on phase-1 transitions.
            gear_drop = self._maybe_drop_gear(discord_id, guild_id, at_boss)

            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="boss_fight",
                details=json.dumps({
                    "boundary": at_boss, "won": True, "risk": risk_tier,
                    "wager": wager, "jc_delta": jc_delta,
                    "stat_point_awarded": stat_point_awarded,
                    "echo_applied": echo_applied,
                    "rounds": round_log,
                }),
            )
            return self._ok(
                won=True,
                phase=(
                    3 if current_status == "phase2_defeated"
                    else 2 if current_status == "phase1_defeated"
                    else None
                ),
                boss_name=boss_name,
                boss_id=boss.boss_id if boss else "",
                boundary=at_boss,
                risk_tier=risk_tier,
                win_chance=round(win_chance, 2),
                jc_delta=jc_delta, payout=jc_delta,
                new_depth=new_depth,
                dialogue=defeat_msg,
                achievements=achievements,
                stat_point_awarded=stat_point_awarded,
                round_log=round_log,
                echo_applied=echo_applied,
                echo_killer_id=(
                    active_echo.get("killer_discord_id")
                    if echo_applied and active_echo else None
                ),
                gear_broken=gear_broken_names,
                gear_drop=gear_drop,
            )

        # Loss branch
        knockback = random.randint(8, 16)
        extra_kb, extra_cd = self._apply_stinger_on_loss(
            discord_id, guild_id, tunnel, boss,
        )
        knockback += extra_kb
        new_depth = max(0, depth - knockback)
        jc_delta = -wager if wager > 0 else 0
        last_dig_effective = now + extra_cd  # extended cooldown pushes the timer forward

        # Persist remaining boss HP so soften-and-retreat works for the
        # state-machine path. ending_boss_hp / boss_hp_max are forwarded
        # from the caller (start_boss_duel / resume_boss_duel) — when the
        # caller didn't track these (legacy auto-resolve path with no HP
        # info), we skip persistence and the next encounter starts fresh.
        bp_for_persist = self._get_boss_progress(tunnel)
        if ending_boss_hp is not None and boss_hp_max is not None:
            self._persist_boss_hp_after_fight(
                bp_for_persist, at_boss, boss.boss_id if boss else "",
                ending_hp=max(0, int(ending_boss_hp)),
                hp_max=max(1, int(boss_hp_max)),
                won=False, outcome="loss", now=int(now),
            )
        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=new_depth,
            boss_progress=json.dumps(bp_for_persist),
            boss_attempts=attempts,
            cheer_data=None,
            last_dig_at=last_dig_effective,
        )
        if wager > 0:
            self.player_repo.add_balance(discord_id, guild_id, -wager)
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="boss_fight",
            details=json.dumps({
                "boundary": at_boss, "won": False, "risk": risk_tier,
                "wager": wager, "knockback": knockback,
                "extra_knockback": extra_kb,
                "extra_cooldown_s": extra_cd,
                "rounds": round_log,
            }),
        )
        return self._ok(
            won=False,
            boss_name=boss_name,
            boss_id=boss.boss_id if boss else "",
            boundary=at_boss,
            risk_tier=risk_tier,
            win_chance=round(win_chance, 2),
            jc_delta=jc_delta,
            knockback=knockback,
            extra_knockback=extra_kb,
            extra_cooldown_s=extra_cd,
            new_depth=new_depth,
            dialogue=f"{boss_name} sends you flying back {knockback} blocks!",
            achievements=[],
            round_log=round_log,
            echo_applied=echo_applied,
            echo_killer_id=(
                active_echo.get("killer_discord_id")
                if echo_applied and active_echo else None
            ),
            gear_broken=gear_broken_names,
            gear_drop=None,
        )

    def retreat_boss(self, discord_id: int, guild_id) -> dict:
        """Retreat from boss. Lose 2-3 blocks and trigger a 30-min cooldown.

        Persisted boss HP from any prior engagement is preserved (the
        retreat exchanges no blows). The cooldown stops the player from
        scout-and-back-off-loop scout the same boss for free intel until it
        expires; ``fight_boss`` / ``start_boss_duel`` / ``scout_boss`` all
        check ``retreat_cooldown_until`` before engaging.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        boss_progress = self._get_boss_progress(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)

        if at_boss is None:
            return self._error("You're not at a boss boundary.")

        loss = random.randint(RETREAT_BLOCK_LOSS_MIN, RETREAT_BLOCK_LOSS_MAX)
        new_depth = max(0, depth - loss)
        now = int(time.time())
        cooldown_until = now + RETREAT_COOLDOWN_SECONDS

        # Mark last_outcome so the next encounter's dialogue uses
        # ``after_retreat`` lines.
        entry = self._read_boss_progress_entry(boss_progress, at_boss)
        entry["last_outcome"] = "retreat"
        entry["first_meet_seen"] = True
        # Preserve boss_id if known.
        bp_raw = boss_progress.get(str(at_boss))
        if isinstance(bp_raw, dict):
            entry.setdefault("boss_id", bp_raw.get("boss_id", ""))
        boss_progress[str(at_boss)] = entry

        self.dig_repo.update_tunnel(
            discord_id, guild_id,
            depth=new_depth,
            boss_progress=json.dumps(boss_progress),
            retreat_cooldown_until=cooldown_until,
        )
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="boss_retreat",
            details=json.dumps({
                "boundary": at_boss, "loss": loss,
                "cooldown_until": cooldown_until,
            }),
        )

        return self._ok(
            boundary=at_boss,
            loss=loss,
            new_depth=new_depth,
            retreat_cooldown_until=cooldown_until,
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

        # Great Lantern is persistent gear — owning one gives the enhanced
        # scout (mechanic pool + stinger warning) and skips lantern consumption.
        has_great_lantern = self.dig_repo.has_great_lantern(discord_id, guild_id)
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        has_lantern = any(i.get("item_type") == "lantern" for i in inventory)
        if not (has_great_lantern or has_lantern):
            return self._error("You need a Lantern to scout the boss.")

        enhanced = has_great_lantern
        if not enhanced:
            # Base lantern is single-use; Great Lantern is persistent.
            self.dig_repo.remove_inventory_item(discord_id, guild_id, "lantern")

        # Calculate odds for all tiers using the HP-duel model.
        prestige_level = tunnel.get("prestige_level", 0) or 0
        _tk = at_boss if at_boss in BOSS_TIER_BONUS else max((k for k in BOSS_TIER_BONUS if k <= at_boss), default=25)
        depth_hit_penalty = BOSS_TIER_BONUS[_tk]["pen"]
        prestige_hit_penalty = BOSS_PRESTIGE_BONUS.get(prestige_level, BOSS_PRESTIGE_BONUS[max(BOSS_PRESTIGE_BONUS)])["pen"]

        cheers = self._get_cheers(tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        payouts = BOSS_PAYOUTS.get(at_boss, (2.0, 3.0, 6.0))

        # Lock the boss before reading boss_id — handles the post-migration
        # case where boss_progress[depth] still has an empty boss_id (the
        # encounter view that normally locks it may be skipped if a caller
        # invokes scout directly).
        scout_boss = self._ensure_boss_locked(discord_id, guild_id, tunnel, at_boss)
        scout_boss_id = scout_boss.boss_id
        active_echo = self.dig_repo.get_active_boss_echo(guild_id, scout_boss_id)
        echo_applied = bool(
            active_echo
            and active_echo.get("killer_discord_id") != discord_id
        )
        # Echo HP discount is applied inside `_scale_boss_stats` now.
        payout_mult = 0.7 if echo_applied else 1.0

        # Apply the player's current gear loadout so previewed odds reflect
        # what they'd actually fight with.
        scout_loadout = self._get_loadout(discord_id, guild_id)

        # Luminosity penalty applies to the previewed odds as well.
        lum_value = self._get_luminosity(tunnel)
        lum_hit_offset, lum_dmg_bonus = _luminosity_combat_penalty(lum_value)

        odds = {}
        for i, tier in enumerate(("cautious", "bold", "reckless")):
            base = BOSS_DUEL_STATS[tier]
            stats = self._apply_gear_to_combat(base, scout_loadout)
            scaled = self._scale_boss_stats(
                stats,
                boss_id=scout_boss_id,
                at_boss=at_boss,
                prestige_level=prestige_level,
                echo_applied=echo_applied,
            )
            boss_hp = int(scaled["boss_hp"])
            boss_hit_chance = float(scaled["boss_hit"])
            boss_dmg_eff = int(scaled["boss_dmg"]) + lum_dmg_bonus

            player_hit = (
                stats["player_hit"]
                - depth_hit_penalty - prestige_hit_penalty
                + cheer_bonus + lum_hit_offset
            )
            player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))
            free_hit = max(
                PLAYER_HIT_FLOOR,
                min(PLAYER_HIT_CEILING, player_hit * BOSS_FREE_FIGHT_ACCURACY_MOD),
            )
            win_pct = _approx_duel_win_prob(
                player_hp=int(stats["player_hp"]),
                boss_hp=boss_hp,
                player_hit=player_hit,
                player_dmg=int(stats["player_dmg"]),
                boss_hit=boss_hit_chance,
                boss_dmg=boss_dmg_eff,
            )
            free_win_pct = _approx_duel_win_prob(
                player_hp=int(stats["player_hp"]),
                boss_hp=boss_hp,
                player_hit=free_hit,
                player_dmg=int(stats["player_dmg"]),
                boss_hit=boss_hit_chance,
                boss_dmg=boss_dmg_eff,
            )
            base_multiplier = payouts[i] if i < len(payouts) else 2.0
            odds[tier] = {
                "win_pct": round(win_pct, 2),
                "free_fight_pct": round(free_win_pct, 2),
                "player_hp": int(stats["player_hp"]),
                "boss_hp": boss_hp,
                "player_hit": round(player_hit, 2),
                "boss_hit": round(boss_hit_chance, 2),
                "multiplier": round(base_multiplier * payout_mult, 2),
            }

        # Resolve the locked boss for richer scout output (and Great Lantern tier).
        from domain.models.boss_mechanics import MECHANIC_REGISTRY as _MECHS
        from domain.models.boss_stingers import STINGER_REGISTRY as _STS
        from services.dig_constants import get_boss_by_id as _get_boss

        boss = _get_boss(scout_boss_id) if scout_boss_id else None
        boss_name = boss.name if boss else BOSS_NAMES.get(at_boss, "Unknown Boss")

        mechanic_pool_preview = None
        stinger_preview = None
        if enhanced and boss is not None:
            mechanic_pool_preview = []
            for mid in boss.mechanic_pool:
                mech = _MECHS.get(mid)
                if mech is None:
                    continue
                mechanic_pool_preview.append({
                    "id": mid,
                    "archetype": mech.archetype,
                    "prompt_title": mech.prompt_title,
                })
            if boss.stinger_id and boss.stinger_id in _STS:
                st = _STS[boss.stinger_id]
                stinger_preview = {
                    "id": st.id,
                    "flavor_on_loss": st.flavor_on_loss,
                    "extra_knockback": st.extra_knockback,
                    "extended_cooldown_s": st.extended_cooldown_s,
                    "cursed_status": st.cursed_status,
                }

        return self._ok(
            boundary=at_boss,
            boss_name=boss_name,
            boss_id=scout_boss_id,
            odds=odds,
            echo_applied=echo_applied,
            echo_killer_id=active_echo.get("killer_discord_id") if echo_applied else None,
            enhanced=enhanced,
            mechanic_pool=mechanic_pool_preview,
            stinger=stinger_preview,
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

        # Cheer has its own short cooldown — independent of the free-dig
        # cooldown so a player who just dug can still cheer for someone else.
        cheerer_tunnel = self.dig_repo.get_tunnel(cheerer_id, guild_id)
        if cheerer_tunnel:
            cheerer_tunnel = dict(cheerer_tunnel)
            last_cheer_at = cheerer_tunnel.get("last_cheer_at") or 0
            elapsed = int(time.time()) - int(last_cheer_at)
            remaining = CHEER_COOLDOWN_SECONDS - elapsed
            if remaining > 0:
                return self._error(f"Cheer cooldown ({remaining}s remaining).")

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

        # Debit cheerer + cheerer cooldown (optional create) + target cheer
        # data commit together. The old flow could charge the cheerer with
        # no cheer actually recorded on the target, or leave the cheerer on
        # no cooldown.
        active_cheers.append({
            "cheerer_id": cheerer_id,
            "expires_at": now + 3600,  # 1h
        })
        self.dig_repo.atomic_cheer_boss(
            cheerer_id=cheerer_id,
            target_id=target_id,
            guild_id=guild_id,
            cost=cost,
            cheerer_last_cheer_at=now,
            create_cheerer_tunnel_name=None if cheerer_tunnel else self.generate_tunnel_name(),
            target_cheer_data_json=json.dumps(active_cheers),
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

        # Tier bosses (25..275) must all be defeated.
        tier_defeated = all(
            (
                (e.get("status") if isinstance(e, dict) else e) == "defeated"
            )
            for b in BOSS_BOUNDARIES
            for e in (boss_progress.get(str(b)),)
            if e is not None
        ) and len([
            b for b in BOSS_BOUNDARIES if boss_progress.get(str(b)) is not None
        ]) == len(BOSS_BOUNDARIES)
        # Pinnacle (depth 300) must also be defeated to ascend.
        pinnacle_entry = boss_progress.get(str(PINNACLE_DEPTH))
        pinnacle_status = (
            pinnacle_entry.get("status") if isinstance(pinnacle_entry, dict)
            else pinnacle_entry
        )
        pinnacle_defeated = pinnacle_status == "defeated"
        all_defeated = tier_defeated and pinnacle_defeated
        at_max = prestige_level >= MAX_PRESTIGE

        can = all_defeated and not at_max
        reason = None
        if not tier_defeated:
            remaining = [
                str(b) for b in BOSS_BOUNDARIES
                if (
                    (boss_progress.get(str(b)) or {}).get("status")
                    if isinstance(boss_progress.get(str(b)), dict)
                    else boss_progress.get(str(b))
                ) != "defeated"
            ]
            reason = f"Bosses remaining: {', '.join(remaining)}"
        elif not pinnacle_defeated:
            reason = "Something stirs deeper still — descend further."
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

        # Reset tunnel — including pinnacle state so the next cycle re-rolls
        # a fresh pinnacle from the rotating pool on first encounter.
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
            pinnacle_boss_id=None,
            pinnacle_phase=0,
            pinnacle_hp_remaining=None,
            pinnacle_last_engaged_at=None,
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

        # Debit + inventory insert commit together so a crash can't leave
        # the player charged with no item added to inventory.
        item_id = self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-price,
            add_inventory_item=item_type,
        )

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

        # Debit (if any) + trap fields commit together.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-cost if cost else 0,
            tunnel_updates={
                "trap_active": 1,
                "trap_free_today": trap_free_today + 1,
                "trap_date": today,
            },
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
        # Debit + insurance window set together: the old two-step flow could
        # leave the player charged with no insurance applied.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-cost,
            tunnel_updates={"insured_until": now + 86400},  # 24h
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
        boss_encounter = False
        boss_info = None

        # Pitch black: force risky (safe option removed)
        if LUMINOSITY_PITCH_FORCE_RISKY and luminosity <= LUMINOSITY_PITCH_BLACK and choice == "safe" and event.get("risky_option"):
            choice = "risky"

        # Handle boon choice — apply selected buff atomically with the
        # audit log so a crash can't record the buff without logging (or
        # vice versa).
        if choice.startswith("boon_") and event.get("boon_options"):
            boon_idx = int(choice.split("_")[1]) if choice.split("_")[1].isdigit() else 0
            boons = event["boon_options"]
            if boon_idx >= len(boons):
                return self._error("Invalid boon selection.")
            boon = boons[boon_idx]
            buff_payload = {
                "id": boon.get("id", "unknown"),
                "name": boon.get("name", "Unknown Buff"),
                "digs_remaining": boon.get("duration_digs", 1),
                "effect": boon.get("effect", {}),
            }
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                tunnel_updates={"temp_buffs": json.dumps(buff_payload)},
                log_detail={
                    "event_id": event_id, "choice": choice,
                    "boon": boon.get("name", boon.get("id")),
                },
                log_action_type="event",
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
            tunnel_updates: dict = {}

            if "jc" in outcome:
                jc_range = outcome["jc"]
                jc_delta = random.randint(jc_range[0], jc_range[1]) if isinstance(jc_range, list) else jc_range
            if "depth" in outcome:
                depth_range = outcome["depth"]
                depth_delta = random.randint(depth_range[0], depth_range[1]) if isinstance(depth_range, list) else depth_range
                if depth_delta > 0:
                    boss_progress = self._get_boss_progress(tunnel)
                    next_boss = self._next_boss_boundary(depth, boss_progress)
                    if next_boss is not None and depth + depth_delta >= next_boss:
                        depth_delta = max(0, next_boss - 1 - depth)
                        boss_encounter = True
                        boss_info = self._build_boss_info(
                            discord_id, guild_id, tunnel, next_boss,
                        )
                tunnel_updates["depth"] = max(0, depth + depth_delta)

            # JC + depth + audit log commit together so a crash can't credit
            # JC without the depth move (or vice versa).
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=jc_delta,
                tunnel_updates=tunnel_updates or None,
                log_detail={
                    "event_id": event_id, "choice": choice,
                    "jc_delta": jc_delta, "depth_delta": depth_delta,
                },
                log_action_type="event",
            )
            return self._ok(event_name=event.get("name", "Unknown Event"), choice=choice,
                            jc_delta=jc_delta, depth_delta=depth_delta, message=message,
                            boss_encounter=boss_encounter, boss_info=boss_info)

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
            # Safe options with no failure defined — cruel echoes creates one.
            # Depth decrement + JC loss + audit log commit together.
            new_depth = max(0, depth - 1)
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=-1,
                tunnel_updates={"depth": new_depth},
                log_detail={"event_id": event_id, "choice": choice, "cruel_echoes": True},
                log_action_type="event",
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

        # Subtle variance on the authored outcome so each fire of a given
        # event differs slightly. JC scales by ±50%, advance shifts ±2,
        # both clamped to preserve sign so a successful outcome never
        # reverses into a retreat. Players see the rolled values; the
        # spread itself stays hidden behind the embed.
        if jc != 0:
            jc = int(round(jc * random.uniform(0.5, 1.5)))
        if advance != 0:
            jittered = advance + random.randint(-2, 2)
            advance = max(1, jittered) if advance > 0 else min(-1, jittered)

        # P7 chain JC multiplier: chained events get 1.5x JC
        if chained and jc > 0:
            chain_mult = ascension.get("chain_jc_multiplier", 1.0)
            if chain_mult > 1.0:
                jc = int(jc * chain_mult)

        if advance > 0:
            boss_progress = self._get_boss_progress(tunnel)
            next_boss = self._next_boss_boundary(depth, boss_progress)
            if next_boss is not None and depth + advance >= next_boss:
                advance = max(0, next_boss - 1 - depth)
                boss_encounter = True
                boss_info = self._build_boss_info(
                    discord_id, guild_id, tunnel, next_boss,
                )

        # Build the tunnel update dict: depth shift + optional temp buff
        # applied on risky/desperate success. Folding them together lets the
        # atomic block touch the tunnel row just once.
        new_depth = max(0, depth + advance)
        tunnel_updates: dict = {}
        if advance != 0:
            tunnel_updates["depth"] = new_depth

        buff_applied = None
        if succeeded and choice in ("risky", "desperate") and event.get("buff_on_success"):
            buff_data = event["buff_on_success"]
            buff_payload = {
                "id": buff_data.get("id", "unknown"),
                "name": buff_data.get("name", "Unknown Buff"),
                "digs_remaining": buff_data.get("duration_digs", 1),
                "effect": buff_data.get("effect", {}),
            }
            tunnel_updates["temp_buffs"] = json.dumps(buff_payload)
            buff_applied = buff_data

        # Splash burns JC from OTHER players; it writes to their rows, not
        # the actor's, so it runs in its own txns around the atomic block.
        splash_result = None
        splash_cfg = event.get("splash")
        if (
            splash_cfg
            and choice in ("risky", "desperate")
            and _splash_trigger_matches(splash_cfg.get("trigger", "failure"), succeeded)
        ):
            from services.dig_splash import (
                resolve_splash,  # local import: keeps dig_service import graph light
            )
            splash_result = resolve_splash(
                player_repo=self.player_repo,
                dig_repo=self.dig_repo,
                guild_id=guild_id,
                digger_id=discord_id,
                event_name=event.get("name", "Unknown Event"),
                strategy=splash_cfg.get("strategy", "random_active"),
                victim_count=int(splash_cfg.get("victim_count", 0)),
                penalty_jc=int(splash_cfg.get("penalty_jc", 0)),
                mode=splash_cfg.get("mode", "burn"),
            )

        # Depth shift + JC credit/debit + optional buff + audit log commit
        # together, so the actor can't be paid without the depth/buff
        # applied (or vice versa).
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=jc,
            tunnel_updates=tunnel_updates or None,
            log_detail={
                "event_id": event_id, "choice": choice, "succeeded": succeeded,
                "advance": advance, "jc": jc, "cave_in": cave_in,
                "splash_victims": (
                    [{"id": vid, "amount": amt} for vid, amt in splash_result.victims]
                    if splash_result else None
                ),
            },
            log_action_type="event",
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
            boss_encounter=boss_encounter,
            boss_info=boss_info,
            splash=_splash_to_dict(splash_result),
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
                if all(
                    (v.get("status") if isinstance(v, dict) else v) == "defeated"
                    for v in bp.values()
                ):
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
        boss_progress = {str(b): "active" for b in BOSS_BOUNDARIES}

        # Reset tunnel + refund + audit log commit together. The old flow
        # could leave the tunnel reset with no refund paid (or vice versa)
        # on a mid-flight crash.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=refund,
            tunnel_updates={
                "depth": 0,
                "boss_progress": json.dumps(boss_progress),
                "boss_attempts": 0,
                "injury_state": None,
                "cheer_data": None,
                "streak_days": 0,
            },
            log_detail={"depth": depth, "refund": refund},
            log_action_type="abandon",
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
