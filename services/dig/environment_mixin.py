"""EnvironmentMixin mixin for :class:`DigService`.

Luminosity, temp buffs, and ascension/corruption/mutations.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import json
import random
import time

from services.dig._common import (
    _luminosity_combat_penalty,
)
from services.dig_constants import (
    ASCENSION_MODIFIERS,
    CORRUPTION_BAD,
    CORRUPTION_WEIRD,
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK,
    LUMINOSITY_DARK_CAVE_IN_BONUS,
    LUMINOSITY_DARK_JC_MULTIPLIER,
    LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP,
    LUMINOSITY_DEEP_DRAIN_START_DEPTH,
    LUMINOSITY_DIM,
    LUMINOSITY_DIM_CAVE_IN_BONUS,
    LUMINOSITY_DRAIN_PER_DIG,
    LUMINOSITY_MAX,
    LUMINOSITY_PITCH_CAVE_IN_BONUS,
    LUMINOSITY_PITCH_JC_MULTIPLIER,
    LUMINOSITY_REFILL_PER_DAY,
    MUTATION_BY_ID,
    MUTATIONS_POOL,
    PINNACLE_DEPTH,
)

_FRACTIONAL_CURSE_EFFECT_CAPS = {
    "cave_in_bonus": 0.10,
    "cooldown_penalty": 0.25,
}


class EnvironmentMixin:
    """EnvironmentMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """
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

    def _apply_luminosity_drain(
        self,
        discord_id: int,
        guild_id,
        tunnel: dict,
        layer_name: str,
        *,
        equipped_gear: dict | None = None,
        persist: bool = True,
    ) -> dict:
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
        pickaxe_tier = self._get_active_pickaxe_tier(
            discord_id,
            guild_id,
            tunnel,
            equipped_gear=equipped_gear,
        )
        if pickaxe_tier >= 6:  # Frostforged or better
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
        if persist:
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

    def _post_pinnacle_decay_factor(
        self,
        depth: int,
        discord_id: int | None = None,
        guild_id=None,
    ) -> float:
        """Per-dig JC and artifact-rate multiplier past the pinnacle.

        Returns 1.0 at or below the pinnacle, then loses 5 percentage
        points per 25 depth beyond it, clamped at 0. Milestone bonuses
        and streak JC are not affected — only the per-dig roll.

        Relics that slow decay (Root Network -25%, Frozen Clock halve)
        scale the rate when ``discord_id`` is provided. Frozen Clock
        supersedes Root Network if both are equipped.
        """
        if depth <= PINNACLE_DEPTH:
            return 1.0
        steps_past = (depth - PINNACLE_DEPTH) // 25
        rate = 0.05
        if discord_id is not None:
            if self._has_relic(discord_id, guild_id, "frozen_clock"):
                rate = 0.025
            elif self._has_relic(discord_id, guild_id, "root_network"):
                rate = 0.0375
        return max(0.0, 1.0 - rate * steps_past)

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

    def _decrement_buff(
        self,
        discord_id: int,
        guild_id,
        tunnel: dict,
        *,
        tunnel_updates: dict | None = None,
    ) -> None:
        """Decrement active buff duration by 1 dig. Clear if expired."""
        buff = self._get_active_buff(tunnel)
        if not buff:
            return
        remaining = buff.get("digs_remaining", 0) - 1
        if remaining <= 0:
            value = None
        else:
            buff["digs_remaining"] = remaining
            value = json.dumps(buff)
        if tunnel_updates is None:
            self.dig_repo.update_tunnel(discord_id, guild_id, temp_buffs=value)
        else:
            tunnel_updates["temp_buffs"] = value
            tunnel["temp_buffs"] = value

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
    # Temp Curses
    # ------------------------------------------------------------------
    # Exact parallel of the temp-buff lifecycle above, operating on the
    # ``temp_curses`` column. Kept separate so a curse and a buff can be
    # active at the same time without overwriting one another.

    def _get_active_curse(self, tunnel: dict) -> dict | None:
        """Get the active temp curse, or None if expired/absent."""
        raw = tunnel.get("temp_curses")
        if not raw:
            return None
        try:
            curse = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return None
        if curse.get("digs_remaining", 0) <= 0:
            return None
        return curse

    def _apply_curse_effects(self, curse: dict | None) -> dict:
        """Extract numeric effects from an active curse. Returns a copy of the
        effect dict so callers cannot mutate the parsed-curse state."""
        if not curse:
            return {}
        return dict(curse.get("effect", {}))

    def _capped_curse_effect(self, effects: dict, key: str) -> float:
        """Return a non-negative fractional curse effect at its active cap."""
        value = effects.get(key, 0.0)
        if not isinstance(value, (int, float)):
            return 0.0
        return max(0.0, min(float(value), _FRACTIONAL_CURSE_EFFECT_CAPS[key]))

    def _decrement_curse(
        self,
        discord_id: int,
        guild_id,
        tunnel: dict,
        *,
        tunnel_updates: dict | None = None,
    ) -> None:
        """Decrement active curse duration by 1 dig. Clear if expired."""
        curse = self._get_active_curse(tunnel)
        if not curse:
            return
        remaining = curse.get("digs_remaining", 0) - 1
        if remaining <= 0:
            value = None
        else:
            curse["digs_remaining"] = remaining
            value = json.dumps(curse)
        if tunnel_updates is None:
            self.dig_repo.update_tunnel(discord_id, guild_id, temp_curses=value)
        else:
            tunnel_updates["temp_curses"] = value
            tunnel["temp_curses"] = value

    def set_temp_curse(self, discord_id: int, guild_id, curse_data: dict) -> None:
        """Set a temp curse on the tunnel (replaces any existing curse)."""
        payload = {
            "id": curse_data.get("id", "unknown"),
            "name": curse_data.get("name", "Unknown Curse"),
            "digs_remaining": curse_data.get("duration_digs", 1),
            "effect": curse_data.get("effect", {}),
        }
        self.dig_repo.update_tunnel(discord_id, guild_id, temp_curses=json.dumps(payload))

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

