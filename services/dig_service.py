"""
Service for the tunnel digging minigame.

Handles all game logic: digging, cave-ins, bosses, prestige,
items, artifacts, sabotage, and traps.
"""

import json
import random
import time

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig._common import (
    RARITY_WEIGHTS,
    _approx_duel_win_prob,
    _luminosity_combat_penalty,
    _prestige_cave_in_multiplier,
    _splash_to_dict,
    _splash_trigger_matches,
    logger,
)
from services.dig.boss_info_mixin import BossInfoMixin
from services.dig.combat_mixin import BossCombatMixin
from services.dig.dig_core_mixin import DigCoreMixin
from services.dig.environment_mixin import EnvironmentMixin
from services.dig.events_mixin import EventsMixin
from services.dig.gear_mixin import GearMixin
from services.dig.pinnacle_mixin import PinnacleMixin
from services.dig.prestige_mixin import PrestigeMixin
from services.dig.progression_mixin import ProgressionMixin
from services.dig_constants import (
    BASE_DIG_JC_PAYOUT_CAP,
    BOSS_BOUNDARIES,
    CAVE_IN_BLOCK_LOSS_RANGES,
    DIG_POSITIVE_JC_MULTIPLIER,
    DIG_STREAK_JC_PAYOUT_CAP,
    EVENT_POOL,
    LAYERS,
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK_EVENT_MULTIPLIER,
    LUMINOSITY_DIM,
    LUMINOSITY_DIM_EVENT_MULTIPLIER,
    LUMINOSITY_MAX,
    LUMINOSITY_PITCH_BLACK,
    LUMINOSITY_PITCH_EVENT_MULTIPLIER,
    MILESTONES,
    PRESTIGE_HARD_CAP,
    STREAKS,
    WEATHER_BY_ID,
    WIN_CHANCE_CAP,
    cave_in_band,
    roll_catastrophic_cave_in,
    scale_positive_dig_jc,
)
from utils.economy_scaling import (
    scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
)

# Public surface plus the module-level names other modules and the dig test
# suite import directly from ``services.dig_service``. The helpers and
# constants are re-exported from ``services.dig._common`` above so those
# imports keep resolving after the package split.
__all__ = [
    "DigService",
    "EVENT_POOL",
    "RARITY_WEIGHTS",
    "WIN_CHANCE_CAP",
    "_approx_duel_win_prob",
    "_get_events_with_art",
    "_luminosity_combat_penalty",
    "_prestige_cave_in_multiplier",
    "_splash_to_dict",
    "_splash_trigger_matches",
    "logger",
]

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


class DigService(
    BossCombatMixin,
    BossInfoMixin,
    PinnacleMixin,
    GearMixin,
    ProgressionMixin,
    EnvironmentMixin,
    DigCoreMixin,
    PrestigeMixin,
    EventsMixin,
):
    """Encapsulates all tunnel digging minigame logic.

    The game logic is split across focused mixins in the ``services.dig``
    package; this class holds the constructor, the ``dig`` /
    ``_compute_preconditions`` entrypoints, and a few cross-cutting helpers,
    and composes the mixins into the single public service object.
    """

    def __init__(
        self,
        dig_repo: DigRepository,
        player_repo: PlayerRepository,
        mana_effects_service=None,
        bankruptcy_repo=None,
        bankruptcy_service=None,
        dig_guild_modifier_repo=None,
        buff_service=None,
        slow_drip_repo=None,
        balance_history_service=None,
        leaderboard_service=None,
        tunnel_naming_service=None,
        inventory_service=None,
        quest_service=None,
        prediction_repo=None,
        protection_service=None,
        curse_repo=None,
        economy_event_service=None,
    ):
        self.dig_repo = dig_repo
        self.player_repo = player_repo
        self.mana_effects_service = mana_effects_service
        self.bankruptcy_repo = bankruptcy_repo
        self.bankruptcy_service = bankruptcy_service
        self.dig_guild_modifier_repo = dig_guild_modifier_repo
        self.buff_service = buff_service
        self.slow_drip_repo = slow_drip_repo
        self.balance_history_service = balance_history_service
        self.prediction_repo = prediction_repo
        self.protection_service = protection_service
        self.curse_repo = curse_repo
        self.economy_event_service = economy_event_service
        # Sub-services for focused concerns. Defaults wire local instances so
        # existing callers (and tests that construct DigService directly) keep
        # working without having to pass anything new.
        from services.dig_inventory_service import DigInventoryService
        from services.dig_leaderboard_service import DigLeaderboardService
        from services.dig_tunnel_naming_service import DigTunnelNamingService

        self.leaderboard_service = leaderboard_service or DigLeaderboardService(
            dig_repo
        )
        self.tunnel_naming_service = (
            tunnel_naming_service or DigTunnelNamingService()
        )
        self.inventory_service = inventory_service or DigInventoryService(
            dig_repo, player_repo
        )
        # Quest progression service. Optional — when None, quest events are
        # treated as always-excluded so the existing flow continues to work
        # in test fixtures and prod paths that haven't wired quests yet.
        self.quest_service = quest_service
        # Process-local cache of equipped relic IDs per (discord_id, guild_id).
        # A single /dig invocation hits ``_has_relic`` ~15 times across yield,
        # cave-in, advance, hazard, and color-dispatch sites; without this
        # cache each call would round-trip to ``dig_repo.get_equipped_relics``.
        # Invalidated on equip/unequip below.
        self._relic_cache: dict[tuple[int, int], frozenset[str]] = {}

    def _apply_daily_economy_reward(self, guild_id: int | None, amount: int) -> int:
        """Apply the active server-wide event to a positive dig reward."""
        if self.economy_event_service is None or amount <= 0:
            return amount
        return self.economy_event_service.adjust_reward(guild_id, amount)

    def _penalize_jc(self, discord_id: int, guild_id, amount: int) -> tuple[int, int]:
        """Apply the bankruptcy debuff to earned dig JC.

        Returns ``(net, penalty)``. When no bankruptcy_service is wired or the
        player is not under penalty, returns the amount unchanged with 0 penalty.
        Only positive winnings are reduced; the penalty is a coin sink. Applies
        regardless of debt (dig income is not garnished).
        """
        if self.bankruptcy_service is None or amount <= 0:
            return amount, 0
        info = self.bankruptcy_service.apply_penalty_to_winnings(
            discord_id, amount, guild_id
        )
        return info["penalized"], info["penalty_applied"]

    def _mana_effects_or_none(self, discord_id: int, guild_id):
        """Resolve the player's active mana effects, swallowing lookup errors.

        Returns ``None`` when there is no service wired, no active mana, or the
        lookup raised — callers should treat that as "no modifiers apply".
        """
        if self.mana_effects_service is None:
            return None
        try:
            effects = self.mana_effects_service.get_effects(discord_id, guild_id)
        except Exception:
            return None
        if effects.color is None:
            return None
        return effects

    def _get_game_date(self) -> str:
        """Get current game date (resets at 4 AM PST). Uses time.time() so tests can mock it."""
        from utils.game_date import get_game_date
        return get_game_date()

    def _get_layer(self, depth: int) -> dict:
        """Return layer info for given depth."""
        for layer in reversed(LAYERS):
            if depth >= layer["min_depth"]:
                return layer
        return LAYERS[0]

    def get_layer(self, depth: int) -> dict:
        """Public: return layer info for given depth."""
        return self._get_layer(depth)

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

    def _apply_auto_buy_for_dig(
        self, discord_id: int, guild_id, tunnel: dict
    ) -> list[dict]:
        """Queue/purchase opted-in consumables for an actual imminent dig."""
        item_types = []
        if int(tunnel.get("auto_buy_hard_hat") or 0):
            item_types.append("hard_hat")
        if int(tunnel.get("auto_buy_torch") or 0):
            item_types.append("torch")
        if not item_types:
            return []
        return self.inventory_service.ensure_auto_buy_items(
            discord_id, guild_id, item_types,
        )

    # ------------------------------------------------------------------
    # Tunnel Name Generation
    # ------------------------------------------------------------------

    def generate_tunnel_name(self) -> str:
        """Random name from 3 pool types (40% adj+noun, 35% title, 25% silly)."""
        return self.tunnel_naming_service.generate_tunnel_name()

    # ------------------------------------------------------------------
    # Core Dig
    # ------------------------------------------------------------------

    def _grant_dig_item_charges(
        self, discord_id: int, guild_id, tunnel: dict, now: int, item_flags: dict,
    ) -> None:
        """Apply the charge-granting consumables resolved for this dig.

        Each grants its effect to both the DB row and the in-memory ``tunnel``
        dict (so the rest of the dig sees it): hard hat (+3 cave-in-prevention
        charges), grappling hook (+5 cushion charges), sonar pulse (primes the
        skip flag for the NEXT dig), reinforcement (48h window), void bait
        (+3 double-event digs). Side effects only.
        """
        # Hard hat: grant 3 charges of full cave-in prevention
        if item_flags["has_hard_hat"]:
            existing_charges = tunnel.get("hard_hat_charges", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                hard_hat_charges=existing_charges + 3,
            )
            tunnel["hard_hat_charges"] = existing_charges + 3

        # Grappling Hook: grant 5 charges that cushion the next cave-ins
        # (zero block_loss + no stun). Stacks across purchases.
        if item_flags["has_grappling_hook"]:
            existing_gh = tunnel.get("grappling_hook_charges", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                grappling_hook_charges=existing_gh + 5,
            )
            tunnel["grappling_hook_charges"] = existing_gh + 5

        # Sonar Pulse: prime the skip flag — it fires on the NEXT dig, not
        # this one (the dig where the item is consumed just sets the flag).
        if item_flags["has_sonar_pulse"]:
            self.dig_repo.update_tunnel(
                discord_id, guild_id, sonar_skip_pending=1,
            )
            tunnel["sonar_skip_pending"] = 1

        # Reinforcement: 48h window — half sabotage damage + cave-in block_loss cap
        if item_flags["has_reinforcement"]:
            reinforced_until_ts = now + 48 * 3600
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                reinforced_until=reinforced_until_ts,
            )

        # Void Bait: double event chance for next 3 digs
        if item_flags["has_void_bait"]:
            existing_vb = tunnel.get("void_bait_digs", 0) or 0
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                void_bait_digs=existing_vb + 3,
            )
            tunnel["void_bait_digs"] = existing_vb + 3

    def _charge_paid_dig_or_block(
        self, discord_id: int, guild_id, tunnel: dict, today: str, paid: bool,
    ) -> tuple[dict | None, int]:
        """Resolve the cooldown / paid-dig gate for a non-first dig.

        Returns ``(block, paid_dig_cost)``: ``block`` is a non-None response
        dict when the dig can't proceed (on cooldown and not paid, or
        insufficient balance) and the caller should surface it; otherwise None
        with ``paid_dig_cost`` (0 when off cooldown). The paid-dig debit and the
        paid-day counter commit atomically here so a crash between the two
        writes can't charge JC without counting the dig.
        """
        cooldown_remaining = self._get_cooldown_remaining(tunnel)
        if cooldown_remaining <= 0:
            return None, 0

        if not paid:
            pc = tunnel.get("paid_digs_today") or 0
            if tunnel.get("paid_dig_date") != today:
                pc = 0
            preview_cost = self._apply_mana_paid_cost_modifier(
                discord_id, guild_id, self._calculate_paid_dig_cost(tunnel, pc),
            )
            return {
                "success": False,
                "error": f"Dig on cooldown ({cooldown_remaining}s remaining).",
                "cooldown_remaining": cooldown_remaining,
                "paid_dig_cost": preview_cost,
                "paid_dig_available": True,
            }, 0

        paid_count = tunnel.get("paid_digs_today") or 0
        if tunnel.get("paid_dig_date") != today:
            paid_count = 0
        paid_dig_cost = self._apply_mana_paid_cost_modifier(
            discord_id, guild_id, self._calculate_paid_dig_cost(tunnel, paid_count),
        )
        balance = self.player_repo.get_balance(discord_id, guild_id)
        if balance < paid_dig_cost:
            return self._error(
                f"Paid dig costs {paid_dig_cost} JC but you only have {balance} JC."
            ), 0

        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-paid_dig_cost,
            tunnel_updates={"paid_dig_date": today, "paid_digs_today": paid_count + 1},
        )
        return None, paid_dig_cost

    def dig(self, discord_id: int, guild_id, paid: bool = False) -> dict:
        """
        Main dig action.

        Returns dict with: success, error, tunnel, depth_before, depth_after,
        advance, jc_earned, milestone_bonus, streak_bonus, cave_in, cave_in_detail,
        boss_encounter, boss_info, event, artifact, is_first_dig,
        items_used, tip.
        """
        # 0. Check player is registered
        if not self.player_repo.exists(discord_id, guild_id):
            return self._error("You need to register first. Use /player register.")

        now = int(time.time())
        today = self._get_game_date()

        overgrowth_active = bool(
            self.buff_service
            and self.buff_service.has_overgrowth(discord_id, guild_id)
        )

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

        # 2. Slow Drip relic: idle income since last dig (credited inline,
        # surfaces only via balance change + audit log).
        self._claim_slow_drip(
            discord_id, guild_id,
            last_dig_at=tunnel.get("last_dig_at"),
        )

        depth_before = tunnel.get("depth", 0)

        # 2a. If the player is already parked at a boss boundary, surface the
        #     boss encounter immediately — cooldown and paid-dig gates would
        #     block access to the Fight button. Re-opening the view awards no
        #     JC, which closes the original farm exploit.
        if not is_first_dig:
            boss_progress_check = self._get_boss_progress(tunnel)
            if self._at_boss_boundary(depth_before, boss_progress_check) is not None:
                parked_return = self._build_parked_boss_return(
                    tunnel, discord_id, guild_id
                )
                if parked_return is not None:
                    return parked_return

        # 2b. Hard cap: the deep refuses to yield further. Block dig
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
            block, paid_dig_cost = self._charge_paid_dig_or_block(
                discord_id, guild_id, tunnel, today, paid,
            )
            if block is not None:
                return block

        # 4. First dig ever: guaranteed safe, welcome info
        if is_first_dig:
            return self._execute_first_dig(
                discord_id, guild_id, tunnel, depth_before, now, today
            )

        auto_purchases = self._apply_auto_buy_for_dig(discord_id, guild_id, tunnel)

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

        # 6. Get queued items and apply effects. The queued rows are NOT deleted
        # here — their ids ride along in ``consumed_item_row_ids`` and are
        # deleted inside the dig's final atomic commit (cave-in or success), so
        # an exception mid-dig can't destroy consumables with nothing to show.
        items_used, items_used_ids, _item_flags, consumed_item_row_ids = (
            self._resolve_queued_items(discord_id, guild_id)
        )
        has_dynamite = _item_flags["has_dynamite"]
        has_lantern = _item_flags["has_lantern"]
        has_torch = _item_flags["has_torch"]
        has_depth_charge = _item_flags["has_depth_charge"]
        has_sonar_pulse = _item_flags["has_sonar_pulse"]
        # hard_hat / grappling_hook / reinforcement / void_bait charge grants
        # are applied via _grant_dig_item_charges below (read from _item_flags).

        # Snapshot the pre-dig Sonar Pulse skip flag BEFORE granting items.
        # Sonar Pulse primes the flag for the *next* dig, not the dig where
        # it's consumed — so the active-this-dig value is whatever was on the
        # tunnel before we touch it.
        sonar_skip_active_this_dig = int(tunnel.get("sonar_skip_pending") or 0) > 0

        self._grant_dig_item_charges(discord_id, guild_id, tunnel, now, _item_flags)

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
        # Yield buff (e.g. Dynamite Cache +75%): a paid consumable that boosts
        # base loot AND lifts the base-payout cap proportionally, so the buff is
        # not silently swallowed by the flat cap.
        buff_yield_mult = float(buff_effects.get("yield_multiplier", 1.0))
        self._decrement_buff(discord_id, guild_id, tunnel)

        # 7d. Get and apply active temp curse (event "curse" threat). Mirrors
        # the buff read/decrement; effects drain rather than boost.
        active_curse = self._get_active_curse(tunnel)
        curse_effects = self._apply_curse_effects(active_curse)
        curse_advance_bonus = curse_effects.get("advance_bonus", 0)
        curse_jc_bonus = curse_effects.get("jc_bonus", 0)
        curse_luminosity_drain = curse_effects.get("luminosity_drain", 0)
        curse_cave_in_bonus = self._capped_curse_effect(
            curse_effects, "cave_in_bonus",
        )
        self._decrement_curse(discord_id, guild_id, tunnel)

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

        # 8e. Apply temp-curse luminosity drain (guttering-light hex). Flat
        # extra light lost — drains even when the layer has no base drain.
        if curse_luminosity_drain > 0:
            luminosity = max(0, luminosity - curse_luminosity_drain)
            lum_info["luminosity_after"] = luminosity
            lum_info["drained"] += curse_luminosity_drain
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

        luminosity = self._apply_lantern_stub_restore(
            discord_id, guild_id, tunnel, lum_info, today,
        )

        pickaxe_tier = self._get_active_pickaxe_tier(discord_id, guild_id, tunnel)
        pickaxe_data = self._get_active_pickaxe_data(discord_id, guild_id, tunnel)
        pickaxe_advance_bonus = pickaxe_data.get("advance_bonus", 0)
        pickaxe_cavein_reduction = pickaxe_data.get("cave_in_reduction", 0)

        perk_fx = self._aggregate_perk_effects(perks)
        perk_cavein_reduction = perk_fx.get("cave_in_reduction", 0.0)
        perk_advance_flat = perk_fx.get("advance_min_bonus", 0.0)
        perk_loot_flat = perk_fx.get("jc_bonus", 0.0)
        perk_advance_bonus = 0.0  # legacy multiplier slot, kept zero
        perk_loot_bonus = 0.0  # legacy multiplier slot, kept zero

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
        # Prism Heart — color-dispatched bonuses (active only with mana)
        prism = self._prism_heart_bonuses(discord_id, guild_id)
        mole_claws_bonus += prism["advance"]
        magma_heart_bonus += prism["jc_flat"]
        miner_stats = self._get_miner_stats(tunnel)
        stat_effects = self._get_stat_effects(miner_stats)

        # 9. Cave-in check (with ascension + corruption + mutation modifiers)
        hard_hat_charges = tunnel.get("hard_hat_charges", 0) or 0
        cave_in_chance = layer.get("cave_in_pct", 0.10)
        # Ascension cave-in bonus
        cave_in_chance += ascension.get("cave_in_bonus", 0)
        cave_in_chance += curse_cave_in_bonus
        shop_curse_stacks = self._shop_curse_stacks(discord_id, guild_id)
        shop_curse_cave_in_bonus = self._shop_curse_cave_in_bonus(
            shop_curse_stacks,
        )
        cave_in_chance += shop_curse_cave_in_bonus
        # Weather cave-in modifier (negated during Storm if Stormcaller equipped)
        weather_cave_in_bonus = weather_fx.get("cave_in_bonus", 0)
        if weather_cave_in_bonus and self._relic_storm_negates_hazard(
            discord_id, guild_id, self._get_weather_code(guild_id, layer_name)
        ):
            weather_cave_in_bonus = 0
        cave_in_chance += weather_cave_in_bonus
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
        cave_in_chance *= _prestige_cave_in_multiplier(prestige_level)
        if overgrowth_active:
            cave_in_chance *= 0.5

        # Silent mana hazard modifier (Forest -, Mountain/Black +).
        cave_in_chance = self._apply_mana_hazard_modifier(
            discord_id, guild_id, cave_in_chance
        )
        # Floor lives below the mana modifier so Forest can't zero out cave-in
        # entirely; thick_skin below intentionally bypasses this.
        cave_in_chance = max(0.01, cave_in_chance)

        # Mutation: thick_skin — first cave-in each day prevented
        thick_skin_saved = False
        if mutation_fx.get("daily_cave_in_shield"):
            shield_date = tunnel.get("thick_skin_date")
            if shield_date != today:
                cave_in_chance = 0.0
                thick_skin_saved = True

        # Hard hat charges prevent cave-in entirely. Each absorb drains
        # 10 luminosity — the helmet keeps you safe, but the cavern remembers.
        # Single atomic update so a crash can't decrement the charge without
        # also paying the luminosity cost.
        if hard_hat_charges > 0:
            cave_in = False
            luminosity = max(0, luminosity - 10)
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                hard_hat_charges=hard_hat_charges - 1,
                luminosity=luminosity,
            )
            lum_info["luminosity_after"] = luminosity
            lum_info["drained"] = lum_info.get("drained", 0) + 10
        else:
            cave_in = random.random() < cave_in_chance
        cave_in_detail = None

        if cave_in:
            # 10. Cave-in consequences
            # Silent Blue mana refund: a fraction of paid_dig_cost comes back
            # to soften the blow. Folded into the same atomic commit as the
            # cave-in balance delta below.
            blue_refund = 0
            if paid_dig_cost > 0 and self.mana_effects_service is not None:
                try:
                    _bf = self.mana_effects_service.get_effects(discord_id, guild_id)
                    if _bf.color is not None and _bf.dig_paid_refund_on_caveins > 0:
                        blue_refund = max(
                            1, int(paid_dig_cost * _bf.dig_paid_refund_on_caveins)
                        )
                except Exception:
                    blue_refund = 0
            band = cave_in_band(depth_before)
            block_min, block_max = CAVE_IN_BLOCK_LOSS_RANGES[band]
            block_loss = random.randint(block_min, block_max)
            # Weather: cap on block loss (e.g. Mudslide Warning)
            weather_loss_cap = weather_fx.get("cave_in_loss_cap")
            if weather_loss_cap is not None:
                block_loss = min(block_loss, int(weather_loss_cap))
            # Weather: extra block loss
            block_loss += int(weather_fx.get("cave_in_loss_bonus", 0))
            # Mutation: brittle_walls — extra block loss
            block_loss += int(mutation_fx.get("cave_in_loss_bonus", 0))
            # Perk: steady_hands reduces depth lost on cave-in
            steady_hands_reduction = perk_fx.get("cave_in_loss_reduction", 0.0)
            if steady_hands_reduction > 0:
                block_loss = max(0, int(block_loss * (1.0 - steady_hands_reduction)))
            # Relic: Patient Stone — -30% depth lost
            if self._has_relic(discord_id, guild_id, "patient_stone"):
                block_loss = max(0, int(block_loss * 0.7))
            # Reinforcement window: cap cave-in block_loss so a single
            # catastrophic roll can't erase a long grind.
            reinforced_until_for_cap = tunnel.get("reinforced_until") or 0
            if now < int(reinforced_until_for_cap):
                block_loss = min(block_loss, 8)
            # Capture pre-grappling block_loss for Gambler's Charm
            block_loss_pre_save = block_loss
            # Grappling hook absorbs the cave-in (zero block_loss + cushion the
            # stun via grappling_absorbed below). Consumes 1 charge.
            grappling_hook_charges = int(tunnel.get("grappling_hook_charges") or 0)
            grappling_absorbed = False
            if grappling_hook_charges > 0:
                block_loss = 0
                grappling_absorbed = True
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    grappling_hook_charges=grappling_hook_charges - 1,
                )
            # Void-Touched pickaxe: salvage 1 block on cave-in
            elif pickaxe_tier >= 7:
                block_loss = max(1, block_loss - 1)
            new_depth = max(0, depth_before - block_loss)
            # Relic: Gambler's Charm — bonus JC equal to 50% of would-have-lost depth
            gamblers_charm_gross_bonus = 0
            if (
                block_loss_pre_save > 0
                and self._has_relic(discord_id, guild_id, "gamblers_charm")
            ):
                gamblers_charm_gross_bonus = max(
                    1, int(block_loss_pre_save * 0.5)
                )

            # Accumulate all cave-in writes into one atomic commit:
            # thick_skin_date, second_wind buff, injury_state, depth delta,
            # counter bump, last_dig_at, cave-in loot credit, medical bill
            # debit, and the audit log all flip together.
            cave_in_tunnel_updates: dict = {
                "depth": new_depth,
                "total_digs": (tunnel.get("total_digs", 0) or 0) + 1,
                "last_dig_at": now,
                "cavein_free_streak": 0,  # Prospector's Streak resets on collapse
            }
            cave_in_balance_delta = blue_refund

            if thick_skin_saved:
                cave_in_tunnel_updates["thick_skin_date"] = today

            # Mutation: cave_in_loot — chance to drop JC on cave-in
            cave_in_jc = 0
            cave_in_gross_jc = 0
            loot_chance = mutation_fx.get("cave_in_loot_chance", 0)
            if loot_chance > 0 and random.random() < loot_chance:
                loot_min = int(mutation_fx.get("cave_in_loot_min", 1))
                loot_max = int(mutation_fx.get("cave_in_loot_max", 3))
                cave_in_gross_jc = random.randint(loot_min, loot_max)

            # Relic: Gambler's Charm — bonus JC for surviving the cave-in
            cave_in_gross_jc += gamblers_charm_gross_bonus
            cave_in_jc = scale_positive_dig_jc(cave_in_gross_jc)
            cave_in_balance_delta += cave_in_jc

            # Mutation: second_wind — flag for next dig advance bonus
            if mutation_fx.get("post_cave_in_advance"):
                cave_in_tunnel_updates["temp_buffs"] = json.dumps({
                    "id": "second_wind", "name": "Second Wind",
                    "digs_remaining": 1,
                    "effect": {"advance_bonus": int(mutation_fx["post_cave_in_advance"])},
                })

            # Mutation: fragile — injuries last longer
            injury_bonus = int(mutation_fx.get("injury_duration_bonus", 0))
            balance = self.player_repo.get_balance(discord_id, guild_id)
            # Grappling hook absorbed: skip consequence roll entirely so the
            # player doesn't get stunned/injured by a cave-in their gear caught.
            if grappling_absorbed:
                catastrophic = False
                cave_in_detail = {
                    "type": "cushioned",
                    "block_loss": 0,
                    "message": "Cave-in! Your grappling line snapped taut and absorbed the impact.",
                }
                jc_debit = 0
            else:
                catastrophic = roll_catastrophic_cave_in(band)
                cave_in_detail, jc_debit = self._apply_cave_in_consequence(
                    discord_id=discord_id,
                    guild_id=guild_id,
                    tunnel=tunnel,
                    depth_before=depth_before,
                    band=band,
                    block_loss=block_loss,
                    catastrophic=catastrophic,
                    balance=balance,
                    injury_bonus=injury_bonus,
                    tunnel_updates=cave_in_tunnel_updates,
                )
                cave_in_balance_delta -= jc_debit
                # Catastrophic depth roll-back overrides new_depth.
                if catastrophic:
                    new_depth = cave_in_tunnel_updates["depth"]

            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=cave_in_balance_delta,
                tunnel_updates=cave_in_tunnel_updates,
                consume_inventory_item_ids=consumed_item_row_ids,
                log_detail={
                    "cave_in": True, "block_loss": block_loss,
                    "detail": cave_in_detail,
                    "depth_before": depth_before, "depth_after": new_depth,
                    "gross_jc": cave_in_gross_jc,
                    "reward_multiplier": (
                        DIG_POSITIVE_JC_MULTIPLIER
                        if cave_in_gross_jc > 0
                        else None
                    ),
                },
                log_action_type="dig",
            )
            if overgrowth_active:
                try:
                    self.buff_service.consume_overgrowth_charge(discord_id, guild_id)
                except Exception:
                    logger.exception("Failed to consume Overgrowth charge")

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
                is_first_dig=False,
                dig_consumed=True,
                items_used=items_used,
                items_used_ids=items_used_ids,
                auto_purchases=auto_purchases,
                pickaxe_tier=pickaxe_tier,
                tip=self._pick_tip(new_depth),
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
        # Relic: Pathfinder's Spur — +1 advance in the deep layers (depth 150+).
        if depth_before >= 150 and self._has_relic(discord_id, guild_id, "pathfinders_spur"):
            advance += 1
        # Temp-curse advance modifier (negative = slowed dig)
        advance += curse_advance_bonus
        # Weather advance modifier
        advance += int(weather_fx.get("advance_bonus", 0))
        # Ascension advance penalty
        advance -= int(ascension.get("advance_penalty", 0))
        # Corruption advance penalty (one-dig)
        if corruption:
            advance -= int(corruption["effects"].get("advance_penalty", 0))
        dynamite_bonus = 5 if has_dynamite else 0
        depth_charge_bonus = 10 if has_depth_charge else 0
        advance = int(advance * (1.0 + perk_advance_bonus) * injury_advance_mod)
        # Half-up rounding so mixed_bonus's 0.5 contribution doesn't get
        # silently truncated to 0 when picked alone.
        advance += int(perk_advance_flat + 0.5)
        # Consumable bonuses are flat - applied after the multiplier so an
        # injury or negative perk can't shrink the advertised +5 / +10.
        advance += dynamite_bonus + depth_charge_bonus
        advance = max(1, advance)

        # 12. Check boss boundary
        boss_progress = self._get_boss_progress(tunnel)
        next_boss = self._next_boss_boundary(boss_progress)
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
        # Ascension JC multiplier + weather JC multiplier; high-prestige
        # ascensions subtract a small layer penalty to dampen normal-dig
        # income (does not affect milestones, perks, or flat bonuses).
        jc_mult = 1.0 + perk_loot_bonus + ascension.get("jc_multiplier", 0) + weather_fx.get("jc_multiplier", 0)
        jc_mult = max(0.0, jc_mult - ascension.get("jc_layer_penalty", 0))
        weather_code_now = self._get_weather_code(guild_id, layer_name)
        relic_yield_mult = self._relic_jc_yield_multiplier(
            discord_id, guild_id, weather_code=weather_code_now,
            luminosity=luminosity,
            is_first_dig_today=self._is_first_dig_of_day(tunnel.get("last_dig_at"), today),
            is_paid_dig=paid_dig_cost > 0,
        )
        # Mana × weather combo: Sunny + White boosts yield.
        weather_combo_yield = 1.0
        if self.mana_effects_service is not None:
            try:
                _wc = self.mana_effects_service.get_weather_combo_modifiers(
                    discord_id, guild_id, weather_code_now,
                )
                weather_combo_yield = _wc["yield_mult"]
            except Exception:
                weather_combo_yield = 1.0
        jc_earned = int(
            jc_earned
            * jc_mult
            * relic_yield_mult
            * weather_combo_yield
            * buff_yield_mult
            * self._luminosity_jc_multiplier(luminosity)
            * self._post_pinnacle_decay_factor(new_depth, discord_id, guild_id)
        ) + magma_heart_bonus + int(perk_loot_flat + 0.5)
        # Weather: flat JC bonus/penalty
        jc_earned += int(weather_fx.get("jc_bonus", 0))
        # Temp-curse JC drain (negative jc_bonus = less JC this dig). The
        # max(0, ...) clamp below still floors a cursed dig at 0 — a curse
        # reduces earnings, it never makes a dig cost money.
        jc_earned += curse_jc_bonus
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

        # Mana variance + steady bonus on base loot only — protects deterministic
        # milestone/streak from a Mountain "zero" roll.
        jc_earned = self._apply_mana_yield_variance(discord_id, guild_id, jc_earned)
        if overgrowth_active:
            jc_earned += 10

        # Relic: Prospector's Streak — flat JC per consecutive cave-in-free dig
        # (capped). Folded into the non-streak total so it counts toward the base
        # cap instead of stacking past it. The counter is bumped here and
        # persisted below; the cave-in branch resets it to 0.
        cavein_free_streak = (tunnel.get("cavein_free_streak", 0) or 0) + 1
        if self._has_relic(discord_id, guild_id, "prospectors_streak"):
            jc_earned += min(cavein_free_streak, 20)

        # Cap the non-streak payout (base loot + relic). Milestones and the
        # daily-streak bonus are separate buckets added on top. A yield buff
        # (Dynamite Cache) lifts the cap proportionally so its boost isn't
        # swallowed — a normal dig still tops out at BASE_DIG_JC_PAYOUT_CAP.
        base_cap = int(BASE_DIG_JC_PAYOUT_CAP * buff_yield_mult)
        jc_earned = min(jc_earned, base_cap)
        # Pre-tax capped non-streak basis — used by the LLM flavor layer to clamp
        # its nudge against the cap without being fooled by later taxes.
        nonstreak_jc = jc_earned

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
        streak, streak_charm_used = self._calculate_daily_streak(
            discord_id, guild_id, tunnel, today
        )

        streak_bonus = 0
        for threshold in sorted(STREAKS.keys(), reverse=True):
            if streak >= threshold:
                streak_bonus = STREAKS[threshold]
                break

        # Perk: patient_step boosts streak JC
        streak_bonus = int(streak_bonus * (1.0 + perk_fx.get("streak_bonus_multiplier", 0.0)))
        streak_bonus = min(streak_bonus, DIG_STREAK_JC_PAYOUT_CAP)

        jc_earned += streak_bonus

        jc_earned = scale_minigame_jc_delta(jc_earned)
        gross_jc = jc_earned
        jc_earned = scale_positive_dig_jc(gross_jc)

        # Plains tithe / Blue tax apply to the scaled full payout (base +
        # milestone + streak) so the transferred/burned amounts are scaled too.
        # (_apply_mana_yield_taxes also applies the daily economy event.)
        jc_earned = self._apply_mana_yield_taxes(discord_id, guild_id, jc_earned)

        # Helltide bell: a flat per-dig tax while the guild modifier is active.
        # Pure deflation — coins burn, not transferred.
        helltide_tax = scale_deflationary_minigame_jc_delta(self._helltide_tax(guild_id))
        if helltide_tax > 0:
            jc_earned = max(0, jc_earned - helltide_tax)

        # Bankruptcy debuff: a penalized digger keeps only the configured
        # fraction of their yield. Applied last (after all yield modifiers) and
        # before the credit; the withheld share is a coin sink.
        jc_earned, dig_bankruptcy_penalty = self._penalize_jc(discord_id, guild_id, jc_earned)

        # 16. Roll for artifact (skip if corruption says so)
        artifact = None
        if not (corruption and corruption["effects"].get("skip_artifact")):
            artifact = self.roll_artifact(
                discord_id, guild_id, new_depth,
                extra_rate_mod=weather_fx.get("artifact_multiplier", 1.0),
            )

        # 17. Roll for random event (layer-specific rates, luminosity, ascension, mutations)
        event_rates = {
            "Dirt": 0.22, "Stone": 0.22, "Crystal": 0.27, "Magma": 0.27,
            "Abyss": 0.31, "Fungal Depths": 0.38, "Frozen Core": 0.31, "The Hollow": 0.45,
        }
        event_chance = event_rates.get(layer_name, 0.22)
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
        sonar_skip_consumed = False
        if random.random() < event_chance:
            event = self.roll_event(
                new_depth, luminosity=luminosity,
                prestige_level=prestige_level,
                discord_id=discord_id, guild_id=guild_id,
                in_boss=boss_encounter, tunnel=tunnel,
                void_bait_active=void_bait_charge_used,
            )
            # Sonar Pulse skip: the player primed Sonar on a prior dig — let
            # this event pass by harmlessly. We still rolled it (so the RNG
            # cadence matches) but suppress the application.
            if event is not None and sonar_skip_active_this_dig:
                event_preview_skipped = {
                    "name": event.get("name"),
                    "description": event.get("description"),
                    "rarity": event.get("rarity", "common"),
                }
                event = None
                sonar_skip_consumed = True
            elif event is not None and self._has_relic(
                discord_id, guild_id, "hollow_eye"
            ):
                # Relic: Hollow Eye — reveal all option outcomes upfront
                event["hollow_eye_revealed"] = True
        else:
            event_preview_skipped = None

        # Lantern + Sonar Pulse: preview what the next event would be. The
        # Lantern variant also surfaces a boss-imminent warning when the
        # player is close to a tier threshold.
        event_preview = None
        boss_scout = None
        if has_lantern or has_sonar_pulse:
            preview = self.roll_event(
                new_depth, luminosity=luminosity,
                prestige_level=prestige_level,
                discord_id=discord_id, guild_id=guild_id,
                in_boss=boss_encounter, tunnel=tunnel,
            )
            if preview:
                event_preview = {
                    "name": preview.get("name"),
                    "description": preview.get("description"),
                    "rarity": preview.get("rarity", "common"),
                }
        if has_lantern:
            # Boss scout: next tier boundary ahead within 10 blocks?
            for boundary in BOSS_BOUNDARIES:
                if boundary > new_depth and boundary - new_depth <= 10:
                    boss_scout = {
                        "blocks_until": boundary - new_depth,
                        "depth": boundary,
                    }
                    break
        if sonar_skip_consumed and event_preview is None:
            # Surface what was skipped even if no preview rolled — gives the
            # player feedback that their Sonar charge fired.
            event_preview = event_preview_skipped

        total_digs = (tunnel.get("total_digs", 0) or 0) + 1

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
            "cavein_free_streak": cavein_free_streak,
            "current_run_jc": run_jc,
            "current_run_artifacts": run_artifacts,
            "current_run_events": run_events_count,
        }
        if void_bait_charge_used:
            final_tunnel_updates["void_bait_digs"] = void_bait_digs - 1
        if sonar_skip_consumed:
            # Clear only when we actually skipped an event this dig. The
            # priming dig sets the flag without clearing, and a primed flag
            # waits across event-less digs until something to skip shows up.
            final_tunnel_updates["sonar_skip_pending"] = 0

        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=jc_earned,
            tunnel_updates=final_tunnel_updates,
            consume_inventory_item_ids=consumed_item_row_ids,
            log_detail={
                "advance": advance, "jc": jc_earned,
                "gross_jc": gross_jc,
                "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
                "depth_before": depth_before, "depth_after": new_depth,
                "boss_encounter": boss_encounter,
                "cave_in": False,
                "corruption": corruption["id"] if corruption else None,
                "streak_charm_used": streak_charm_used,
            },
            log_action_type="dig",
        )
        # Blood Pact: an active pact on this digger skims a share of the dig
        # payout to the pact holder. Dig is the primary earnings source, so this
        # is where the shop's advertised "skim of the target's earnings" mostly
        # applies.
        jc_earned = self._apply_blood_pact_skim_to_payout(discord_id, guild_id, jc_earned)
        if overgrowth_active:
            try:
                self.buff_service.consume_overgrowth_charge(discord_id, guild_id)
            except Exception:
                logger.exception("Failed to consume Overgrowth charge")

        # 22. Return result
        return self._ok(
            tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
            depth_before=depth_before,
            depth_after=new_depth,
            advance=advance,
            jc_earned=jc_earned,
            gross_jc=gross_jc,
            nonstreak_jc=nonstreak_jc,
            nonstreak_cap=base_cap,
            bankruptcy_penalty=dig_bankruptcy_penalty,
            milestone_bonus=milestone_bonus,
            streak_bonus=streak_bonus,
            cave_in=False,
            cave_in_detail=None,
            boss_encounter=boss_encounter,
            boss_info=boss_info,
            has_lantern=has_lantern,
            event=event,
            artifact=artifact,
            is_first_dig=False,
            dig_consumed=True,
            items_used=items_used,
            items_used_ids=items_used_ids,
            auto_purchases=auto_purchases,
            pickaxe_tier=pickaxe_tier,
            tip=self._pick_tip(new_depth),
            luminosity_info=lum_info,
            paid_cost=paid_dig_cost if paid_dig_cost > 0 else 0,
            dynamite_bonus=dynamite_bonus,
            corruption=corruption,
            mutations=[m.get("name") for m in mutations] if mutations else None,
            event_preview=event_preview,
            boss_scout=boss_scout,
            sonar_skipped=sonar_skip_consumed,
            weather=weather_info,
            streak_charm_used=streak_charm_used,
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

        overgrowth_active = bool(
            self.buff_service
            and self.buff_service.has_overgrowth(discord_id, guild_id)
        )

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

        depth_before = tunnel.get("depth", 0)

        # Parked-at-boss short-circuit: surface the encounter before the
        # cooldown gate so Fight/Retreat buttons are always reachable.
        if not is_first_dig:
            boss_progress_check = self._get_boss_progress(tunnel)
            if self._at_boss_boundary(depth_before, boss_progress_check) is not None:
                parked_return = self._build_parked_boss_return(
                    tunnel, discord_id, guild_id,
                )
                if parked_return is not None:
                    return parked_return, None

        # Cooldown / paid dig check — normal digs only.
        paid_dig_cost = 0
        if not is_first_dig:
            block, paid_dig_cost = self._charge_paid_dig_or_block(
                discord_id, guild_id, tunnel, today, paid,
            )
            if block is not None:
                return block, None

        if is_first_dig:
            return self._execute_first_dig(
                discord_id, guild_id, tunnel, depth_before, now, today,
            ), None

        auto_purchases = self._apply_auto_buy_for_dig(discord_id, guild_id, tunnel)

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

        # Queued items. Read-only: the rows are deleted by apply_dig_outcome's
        # atomic commit (ids threaded through the preconditions dict), so a
        # failure between this call and the outcome commit can't destroy them.
        items_used, items_used_ids, _item_flags, consumed_item_row_ids = (
            self._resolve_queued_items(discord_id, guild_id)
        )
        has_dynamite = _item_flags["has_dynamite"]
        has_hard_hat = _item_flags["has_hard_hat"]
        has_lantern = _item_flags["has_lantern"]
        has_torch = _item_flags["has_torch"]
        has_grappling_hook = _item_flags["has_grappling_hook"]
        has_depth_charge = _item_flags["has_depth_charge"]
        has_sonar_pulse = _item_flags["has_sonar_pulse"]

        # Sonar Pulse flag persists across digs — snapshot pre-grant.
        sonar_skip_active_this_dig = int(tunnel.get("sonar_skip_pending") or 0) > 0

        self._grant_dig_item_charges(discord_id, guild_id, tunnel, now, _item_flags)

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

        # Temp curse (event "curse" threat) — read/decrement mirrors the buff.
        active_curse = self._get_active_curse(tunnel)
        curse_effects = self._apply_curse_effects(active_curse)
        curse_advance_bonus = curse_effects.get("advance_bonus", 0)
        curse_jc_bonus = curse_effects.get("jc_bonus", 0)
        curse_luminosity_drain = curse_effects.get("luminosity_drain", 0)
        curse_cave_in_bonus = self._capped_curse_effect(
            curse_effects, "cave_in_bonus",
        )
        self._decrement_curse(discord_id, guild_id, tunnel)

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

        # Temp-curse luminosity drain (guttering-light hex) — flat extra light.
        if curse_luminosity_drain > 0:
            luminosity = max(0, luminosity - curse_luminosity_drain)
            lum_info["luminosity_after"] = luminosity
            lum_info["drained"] += curse_luminosity_drain
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)

        luminosity = self._apply_lantern_stub_restore(
            discord_id, guild_id, tunnel, lum_info, today,
        )

        pickaxe_tier = self._get_active_pickaxe_tier(discord_id, guild_id, tunnel)
        pickaxe_data = self._get_active_pickaxe_data(discord_id, guild_id, tunnel)
        pickaxe_advance_bonus = pickaxe_data.get("advance_bonus", 0)
        pickaxe_cavein_reduction = pickaxe_data.get("cave_in_reduction", 0)

        perk_fx = self._aggregate_perk_effects(perks)
        perk_cavein_reduction = perk_fx.get("cave_in_reduction", 0.0)
        perk_advance_flat = perk_fx.get("advance_min_bonus", 0.0)
        perk_loot_flat = perk_fx.get("jc_bonus", 0.0)
        perk_advance_bonus = 0.0  # legacy multiplier slot, kept zero
        perk_loot_bonus = 0.0  # legacy multiplier slot, kept zero

        if "deep_sight" in perks and lum_info.get("drained", 0) > 0:
            restored = max(1, lum_info["drained"] // 4)
            luminosity = min(LUMINOSITY_MAX, luminosity + restored)
            lum_info["luminosity_after"] = luminosity
            self.dig_repo.update_tunnel(discord_id, guild_id, luminosity=luminosity)
            tunnel["luminosity"] = luminosity

        relic_cavein_mod = 0.97 if self._has_relic(discord_id, guild_id, "crystal_compass") else 1.0
        mole_claws_bonus = 1 if self._has_relic(discord_id, guild_id, "mole_claws") else 0
        magma_heart_bonus = 1 if self._has_relic(discord_id, guild_id, "magma_heart") else 0
        # Prism Heart — color-dispatched bonuses (active only with mana)
        prism = self._prism_heart_bonuses(discord_id, guild_id)
        mole_claws_bonus += prism["advance"]
        magma_heart_bonus += prism["jc_flat"]
        miner_stats = self._get_miner_stats(tunnel)
        stat_effects = self._get_stat_effects(miner_stats)

        # ── Cave-in chance ────────────────────────────────────────
        hard_hat_charges = tunnel.get("hard_hat_charges", 0) or 0
        grappling_hook_charges = int(tunnel.get("grappling_hook_charges") or 0)
        cave_in_chance = layer.get("cave_in_pct", 0.10)
        cave_in_chance += ascension.get("cave_in_bonus", 0)
        cave_in_chance += curse_cave_in_bonus
        shop_curse_stacks = self._shop_curse_stacks(discord_id, guild_id)
        shop_curse_cave_in_bonus = self._shop_curse_cave_in_bonus(
            shop_curse_stacks,
        )
        cave_in_chance += shop_curse_cave_in_bonus
        weather_cave_in_bonus = weather_fx.get("cave_in_bonus", 0)
        if weather_cave_in_bonus and self._relic_storm_negates_hazard(
            discord_id, guild_id, self._get_weather_code(guild_id, layer_name)
        ):
            weather_cave_in_bonus = 0
        cave_in_chance += weather_cave_in_bonus
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
        cave_in_chance *= _prestige_cave_in_multiplier(prestige_level)
        if overgrowth_active:
            cave_in_chance *= 0.5

        # Silent mana hazard modifier (Forest -, Mountain/Black +).
        cave_in_chance = self._apply_mana_hazard_modifier(
            discord_id, guild_id, cave_in_chance
        )
        # Floor lives below the mana modifier so Forest can't zero out cave-in
        # entirely; thick_skin below intentionally bypasses this.
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
            adv_fixed += 10

        adv_mult = (1.0 + perk_advance_bonus) * injury_advance_mod
        advance_min = max(1, int((base_adv_min + adv_fixed) * adv_mult)) + int(perk_advance_flat + 0.5)
        advance_max = max(1, int((base_adv_max + adv_fixed) * adv_mult)) + int(perk_advance_flat + 0.5)

        # ── Effective JC range ────────────────────────────────────
        jc_min_base = layer.get("jc_min", 1)
        jc_max_base = layer.get("jc_max", 3)
        jc_mult = (
            1.0
            + perk_loot_bonus
            + ascension.get("jc_multiplier", 0)
            + weather_fx.get("jc_multiplier", 0)
        )
        jc_mult = max(0.0, jc_mult - ascension.get("jc_layer_penalty", 0))
        jc_mult *= self._luminosity_jc_multiplier(luminosity)
        jc_mult *= self._post_pinnacle_decay_factor(depth_before, discord_id, guild_id)
        # Relic yield (deterministic only — preview shows static range)
        jc_mult *= self._relic_jc_yield_multiplier(
            discord_id, guild_id,
            weather_code=self._get_weather_code(guild_id, layer_name),
            luminosity=luminosity,
            is_first_dig_today=self._is_first_dig_of_day(tunnel.get("last_dig_at"), today),
            is_paid_dig=paid_dig_cost > 0,
            include_random=False,
        )
        if depth_before >= 276:
            jc_mult *= 0.83
        jc_fixed = magma_heart_bonus + int(weather_fx.get("jc_bonus", 0)) + int(perk_loot_flat + 0.5)
        jc_min = max(0, int(jc_min_base * jc_mult) + jc_fixed)
        jc_max = max(0, int(jc_max_base * jc_mult) + jc_fixed)
        if corruption and corruption["effects"].get("fixed_jc") is not None:
            jc_min = jc_max = corruption["effects"]["fixed_jc"]

        # ── Event chance + eligible events ────────────────────────
        event_rates = {
            "Dirt": 0.22, "Stone": 0.22, "Crystal": 0.27, "Magma": 0.27,
            "Abyss": 0.31, "Fungal Depths": 0.38, "Frozen Core": 0.31,
            "The Hollow": 0.45,
        }
        event_chance = event_rates.get(layer_name, 0.22)
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
        # Quest events are excluded here too, mirroring roll_event/_chain_event,
        # so the LLM context never sees quest event ids it shouldn't suggest.
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
            and not e.get("quest_id")
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

        jc_min = min(jc_min, BASE_DIG_JC_PAYOUT_CAP)
        jc_max = min(jc_max, BASE_DIG_JC_PAYOUT_CAP)

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

        # Re-clamp after social modifiers, unless thick_skin intentionally
        # zeroed cave-in for the day (the floor would otherwise undo it).
        if not thick_skin_saved:
            cave_in_chance = max(0.01, cave_in_chance)
        event_chance = min(event_chance, 0.75)

        preconditions = {
            "discord_id": discord_id,
            "guild_id": guild_id,
            "now": now,
            "today": today,
            "tunnel": tunnel,
            "depth_before": depth_before,
            "injury_advance_mod": injury_advance_mod,
            "items_used": items_used,
            "items_used_ids": items_used_ids,
            "consumed_item_row_ids": consumed_item_row_ids,
            "auto_purchases": auto_purchases,
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
            "curse_advance_bonus": curse_advance_bonus,
            "curse_jc_bonus": curse_jc_bonus,
            "shop_curse_stacks": shop_curse_stacks,
            "shop_curse_cave_in_bonus": shop_curse_cave_in_bonus,
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
            "perk_advance_flat": perk_advance_flat,
            "perk_loot_flat": perk_loot_flat,
            "perk_fx": perk_fx,
            "mole_claws_bonus": mole_claws_bonus,
            "magma_heart_bonus": magma_heart_bonus,
            "miner_stats": miner_stats,
            "stat_effects": stat_effects,
            "hard_hat_charges": hard_hat_charges,
            "hard_hat_prevents": hard_hat_prevents,
            "grappling_hook_charges": grappling_hook_charges,
            "sonar_skip_active_this_dig": sonar_skip_active_this_dig,
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
            "overgrowth_active": overgrowth_active,
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
