"""EventsMixin mixin for :class:`DigService`.

Random event chaining, rolling, and resolving.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import json
import random
import time
import uuid

import services.dig_service as dig_service
from services.dig._common import (
    RARITY_WEIGHTS,
    _splash_to_dict,
    _splash_trigger_matches,
    logger,
)
from services.dig_constants import (
    CURSE_DURATION_BONUS_DIGS,
    CURSE_STRENGTH_MULT,
    EVENT_CHAIN_CHANCE,
    LUMINOSITY_DARK_RISKY_PENALTY,
    LUMINOSITY_DIM,
    LUMINOSITY_MAX,
    LUMINOSITY_PITCH_BLACK,
    LUMINOSITY_PITCH_FORCE_RISKY,
    NEGATIVE_EVENT_JC_MULTIPLIER,
    UNIQUE_GEAR,
    strengthen_dig_event_penalty,
)
from services.dig_data.event_types import scale_curse_effects
from utils.economy_scaling import (
    scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
)

SHOP_CURSE_EVENT_RISK_WEIGHT_PER_STACK = 0.50
SHOP_CURSE_RISKY_PENALTY_PER_STACK = 0.05
SHOP_CURSE_RISKY_PENALTY_CAP = 0.15
SHOP_CURSE_CAVE_IN_BONUS_PER_STACK = 0.02
SHOP_CURSE_CAVE_IN_BONUS_CAP = 0.06


class EventsMixin:
    """EventsMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """
    @staticmethod
    def _event_has_risk(event: dict) -> bool:
        """True if any outcome can harm the digger.

        Used by the ``veteran_miner`` perk to decide whether a successful
        resolution earns the risky-success bonus. Covers both the new-style
        safe/risky/desperate option dicts and the legacy ``outcomes`` map.
        """
        def _has_negative_value(value) -> bool:
            if isinstance(value, list):
                return any(
                    isinstance(item, (int, float)) and item < 0
                    for item in value
                )
            return isinstance(value, (int, float)) and value < 0

        def _is_negative(payload: dict | None) -> bool:
            if not payload:
                return False
            return (
                _has_negative_value(payload.get("jc"))
                or _has_negative_value(payload.get("advance"))
                or bool(payload.get("cave_in"))
                or int(payload.get("streak_loss", 0) or 0) > 0
                or bool(payload.get("curse"))
            )

        for opt_key in ("safe_option", "risky_option", "desperate_option"):
            opt = event.get(opt_key)
            if not opt:
                continue
            if _is_negative(opt.get("success")) or _is_negative(opt.get("failure")):
                return True
        return any(_is_negative(outcome) for outcome in (event.get("outcomes") or {}).values())

    def _shop_curse_stacks(self, discord_id: int, guild_id) -> int:
        """Return the number of active shop-bought curses on a digger."""
        curse_repo = getattr(self, "curse_repo", None)
        if curse_repo is None:
            return 0
        try:
            return max(0, int(curse_repo.count_active_curses_for_target(
                discord_id, guild_id, int(time.time()),
            )))
        except Exception:
            logger.debug("shop curse lookup failed during dig", exc_info=True)
            return 0

    @staticmethod
    def _shop_curse_cave_in_bonus(stack_count: int) -> float:
        return min(
            SHOP_CURSE_CAVE_IN_BONUS_CAP,
            max(0, stack_count) * SHOP_CURSE_CAVE_IN_BONUS_PER_STACK,
        )

    def _chain_event(self, depth: int, prestige_level: int,
                     trigger_rarity: str,
                     trigger_event_id: str | None = None) -> dict | None:
        """P7+: 25% chance to chain another event of same or higher rarity.

        Deterministic-chain override: if the resolving event has a
        ``next_event_id`` and the player meets that next event's
        ``min_prestige``, fire it deterministically — used by
        lore-driven multi-step arcs.
        """
        if trigger_event_id:
            trigger_def = next(
                (e for e in dig_service.EVENT_POOL if e["id"] == trigger_event_id), None,
            )
            chain_id = trigger_def.get("next_event_id") if trigger_def else None
            if chain_id:
                next_def = next(
                    (e for e in dig_service.EVENT_POOL if e["id"] == chain_id), None,
                )
                if next_def and prestige_level >= next_def.get("min_prestige", 0):
                    return {
                        "id": next_def["id"],
                        "name": next_def["name"],
                        "description": next_def["description"],
                        "complexity": next_def.get("complexity", "choice"),
                        "safe_option": next_def.get("safe_option"),
                        "risky_option": next_def.get("risky_option"),
                        "desperate_option": next_def.get("desperate_option"),
                        "boon_options": next_def.get("boon_options"),
                        "buff_on_success": next_def.get("buff_on_success"),
                        "rarity": next_def.get("rarity", "common"),
                        "chained": True,
                    }

        if prestige_level < 7:
            return None
        if random.random() >= EVENT_CHAIN_CHANCE:
            return None
        rarity_order = ["common", "uncommon", "rare", "legendary"]
        min_idx = rarity_order.index(trigger_rarity) if trigger_rarity in rarity_order else 0
        allowed_rarities = set(rarity_order[min_idx:])
        eligible = [
            e for e in dig_service.EVENT_POOL
            if depth >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth <= e["max_depth"])
            and e.get("rarity", "common") in allowed_rarities
            and prestige_level >= e.get("min_prestige", 0)
            and not e.get("chain_only", False)
            # Quest events ride only the primary roll_event filter so we don't
            # leak quest flavor to players who aren't on the matching stage.
            and not e.get("quest_id")
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

    def roll_event(self, depth: int, luminosity: int = 100,
                   prestige_level: int = 0,
                   *,
                   discord_id: int | None = None,
                   guild_id: int | None = None,
                   in_boss: bool = False,
                   tunnel: dict | None = None,
                   void_bait_active: bool = False) -> dict | None:
        """
        Roll for a random event with layer-specific rates, rarity, and prestige gating.

        Returns event info dict, or None if no event triggers.

        ``discord_id``/``guild_id`` and ``in_boss`` are optional player context
        used to filter quest-tagged events. When omitted, all quest events are
        excluded (preserves backward compatibility for tests / non-player call
        sites). When supplied, only the player's current eligible quest stage
        event (or the stage-1 event of every starter they qualify for, if
        idle) competes in the pool — and never during boss-fight digs.

        ``tunnel`` is forwarded to the quest eligibility check to avoid a
        second DB fetch when the caller already has it in scope.
        """
        layer = self._get_layer(depth)
        layer_name = layer.get("name", "Dirt")
        is_pitch_black = luminosity <= 0
        ascension = self._get_ascension_effects(prestige_level)

        # Resolve the set of quest event ids this player is currently allowed
        # to roll. Quest events are excluded entirely during a boss-fight dig
        # or when no quest_service / no player context is available.
        # ``getattr`` defends against tests that bypass __init__ via __new__.
        eligible_quest_ids: set[str] = set()
        quest_service = getattr(self, "quest_service", None)
        if (
            not in_boss
            and quest_service is not None
            and discord_id is not None
        ):
            try:
                eligible_quest_ids = quest_service.eligible_quest_event_ids(
                    discord_id, guild_id, tunnel=tunnel,
                )
            except Exception:
                logger.debug("quest eligibility resolution failed", exc_info=True)
                eligible_quest_ids = set()

        # Filter eligible events by depth, layer, darkness, prestige, and
        # chain-only flag (chain_only events are reachable only via
        # deterministic chain from a predecessor, never the random pool).
        # Quest events are filtered to the player's currently eligible set.
        eligible = [
            e for e in dig_service.EVENT_POOL
            if depth >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth <= e["max_depth"])
            and (e.get("layer") is None or e["layer"] == layer_name)
            and (not e.get("requires_dark") or is_pitch_black)
            and prestige_level >= e.get("min_prestige", 0)
            and not e.get("chain_only", False)
            and (
                not e.get("quest_id")
                or e["id"] in eligible_quest_ids
            )
        ]

        # Non-darkness events are excluded at pitch black if darkness events exist
        if is_pitch_black:
            dark_events = [e for e in eligible if e.get("requires_dark")]
            if dark_events:
                eligible = dark_events + [e for e in eligible if not e.get("requires_dark")]

        if not eligible:
            return None

        # Rarity-weighted selection with ascension modifiers + Void Bait bias
        rare_mult = 1.0 + ascension.get("rare_event_multiplier", 0)
        legendary_mult = 1.0 + ascension.get("legendary_event_multiplier", 0)
        if void_bait_active:
            rare_mult *= 1.25
            legendary_mult *= 1.5
        adjusted_weights = dict(RARITY_WEIGHTS)
        adjusted_weights["rare"] = int(RARITY_WEIGHTS["rare"] * rare_mult)
        adjusted_weights["legendary"] = int(RARITY_WEIGHTS["legendary"] * legendary_mult)

        shop_curse_stacks = (
            self._shop_curse_stacks(discord_id, guild_id)
            if discord_id is not None else 0
        )
        weighted = []
        for event in eligible:
            weight = adjusted_weights.get(event.get("rarity", "common"), 70)
            if shop_curse_stacks and self._event_has_risk(event):
                weight = int(weight * (
                    1.0 + SHOP_CURSE_EVENT_RISK_WEIGHT_PER_STACK * shop_curse_stacks
                ))
            weighted.append((event, weight))
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
        event = next((e for e in dig_service.EVENT_POOL if e["id"] == event_id), None)
        if event is None:
            return self._error("Unknown event.")

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        active_temp_curse = self._get_active_curse(tunnel)
        depth = tunnel.get("depth", 0)
        luminosity = tunnel.get("luminosity", LUMINOSITY_MAX)
        prestige_level = tunnel.get("prestige_level", 0) or 0
        ascension = self._get_ascension_effects(prestige_level)
        perks = self._get_prestige_perks(tunnel)
        perk_fx = self._aggregate_perk_effects(perks)
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
                    next_boss = self._next_boss_boundary(boss_progress)
                    if next_boss is not None and depth + depth_delta >= next_boss:
                        depth_delta = max(0, next_boss - 1 - depth)
                        boss_encounter = True
                        boss_info = self._build_boss_info(
                            discord_id, guild_id, tunnel, next_boss,
                        )
                tunnel_updates["depth"] = max(0, depth + depth_delta)
            jc_delta = scale_minigame_jc_delta(jc_delta)

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

        # Relic: Diviner's Knot — +10% success on risky choices.
        if choice == "risky" and self._has_relic(discord_id, guild_id, "diviners_knot"):
            success_chance = min(1.0, success_chance + 0.10)

        if (
            choice == "risky"
            and active_temp_curse is not None
            and self._has_relic(discord_id, guild_id, "black_wax_seal")
        ):
            success_chance = min(1.0, success_chance + 0.05)

        # Dark luminosity: risky/desperate options are harder
        if choice in ("risky", "desperate") and luminosity < LUMINOSITY_DIM:
            success_chance = max(0.05, success_chance - LUMINOSITY_DARK_RISKY_PENALTY)

        shop_curse_stacks = self._shop_curse_stacks(discord_id, guild_id)
        if choice in ("risky", "desperate") and shop_curse_stacks:
            shop_curse_penalty = min(
                SHOP_CURSE_RISKY_PENALTY_CAP,
                shop_curse_stacks * SHOP_CURSE_RISKY_PENALTY_PER_STACK,
            )
            success_chance = max(0.05, success_chance - shop_curse_penalty)

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

        gear_definition = None
        gear_reward_pool = result.get("gear_reward_pool") or ()
        eligible_gear = [
            UNIQUE_GEAR[item_id]
            for item_id in gear_reward_pool
            if item_id in UNIQUE_GEAR
        ]
        if eligible_gear:
            gear_definition = random.choice(eligible_gear)

        advance = result.get("advance", 0)
        jc = result.get("jc", 0)
        # Negative-event tuning: flat JC losses on a failure bite a bit harder.
        # Positive payouts are untouched (their own bonuses are applied below).
        if jc < 0:
            jc = int(round(jc * NEGATIVE_EVENT_JC_MULTIPLIER))
        cave_in = result.get("cave_in", False)
        streak_loss = result.get("streak_loss", 0) or 0
        curse = result.get("curse")
        description = result.get("description", "Something happened.")

        # Subtle variance on the authored outcome so each fire of a given
        # event differs slightly. JC scales by ±50%, advance shifts ±2,
        # both clamped to preserve sign so a successful outcome never
        # reverses into a retreat. Players see the rolled values; the
        # spread itself stays hidden behind the embed.
        if jc != 0:
            jc = int(round(jc * random.uniform(0.5, 1.5)))
        if jc < 0:
            jc = strengthen_dig_event_penalty(jc)
        if advance != 0:
            jittered = advance + random.randint(-2, 2)
            advance = max(1, jittered) if advance > 0 else min(-1, jittered)

        if (
            succeeded
            and choice == "safe"
            and self._has_relic(discord_id, guild_id, "chipped_compass")
        ):
            advance += 1

        if self._has_relic(discord_id, guild_id, "burning_ledger"):
            if jc > 0:
                jc = int(round(jc * 1.15))
            elif jc < 0:
                jc = int(round(jc * 1.25))

        # P7 chain JC multiplier: chained events get 1.5x JC
        if chained and jc > 0:
            chain_mult = ascension.get("chain_jc_multiplier", 1.0)
            if chain_mult > 1.0:
                jc = int(jc * chain_mult)

        # Perk: tunnel_mastery — chained events pay more (per-stack additive).
        if chained and jc > 0:
            tm_bonus = perk_fx.get("expedition_reward_bonus", 0.0)
            if tm_bonus > 0:
                jc = int(jc * (1.0 + tm_bonus))

        # Perk: veteran_miner — events with possible negative outcomes pay
        # more on a successful (positive-JC) resolution.
        if jc > 0 and self._event_has_risk(event):
            vm_bonus = perk_fx.get("risky_success_bonus", 0.0)
            if vm_bonus > 0:
                jc = int(jc * (1.0 + vm_bonus))

        if advance > 0:
            boss_progress = self._get_boss_progress(tunnel)
            next_boss = self._next_boss_boundary(boss_progress)
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

        black_wax_seal_spent = False
        if (
            succeeded
            and choice == "risky"
            and active_temp_curse is not None
            and self._has_relic(discord_id, guild_id, "black_wax_seal")
        ):
            remaining_curse = dict(active_temp_curse)
            remaining = int(remaining_curse.get("digs_remaining", 0)) - 1
            if remaining > 0:
                remaining_curse["digs_remaining"] = remaining
                tunnel_updates["temp_curses"] = json.dumps(remaining_curse)
            else:
                tunnel_updates["temp_curses"] = None
            black_wax_seal_spent = True

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

        # Streak threat — a failed risky pick can knock days off the daily
        # streak. The setback is partial and scales with streak length
        # (every 20 days adds +1), so it bites mid-streak players hardest
        # and only nicks 30+ day buffers. NEVER a full reset.
        streak_days_lost = 0
        if streak_loss > 0:
            current_streak = tunnel.get("streak_days", 0) or 0
            setback = streak_loss + (current_streak // 20)
            new_streak = max(0, current_streak - setback)
            streak_days_lost = current_streak - new_streak
            tunnel_updates["streak_days"] = new_streak

        # Curse threat — a failed risky pick can apply a lingering hex over
        # the next few digs. Written to the dedicated temp_curses column so
        # it never clobbers an active temp buff.
        curse_applied = None
        if isinstance(curse, dict):
            # Curses bite harder: scale the harmful effect magnitudes (every
            # value in a curse is a penalty, so sign is preserved) and extend
            # the duration. Build a fresh effect dict so the shared event
            # definition isn't mutated across fires.
            base_effect = curse.get("effect", {})
            scaled_effect = scale_curse_effects(
                base_effect,
                multiplier=CURSE_STRENGTH_MULT,
            )
            curse_payload = {
                "id": curse.get("id", "unknown"),
                "name": curse.get("name", "Unknown Curse"),
                "digs_remaining": curse.get("duration_digs", 1) + CURSE_DURATION_BONUS_DIGS,
                "effect": scaled_effect,
            }
            tunnel_updates["temp_curses"] = json.dumps(curse_payload)
            curse_applied = curse

        # Marquee guild modifier (e.g. helltide_active) — set on success.
        guild_modifier_set: dict | None = None
        gm_cfg = event.get("guild_modifier_on_success")
        if (
            gm_cfg
            and succeeded
            and choice in ("risky", "desperate")
            and self.dig_guild_modifier_repo is not None
        ):
            try:
                self.dig_guild_modifier_repo.set_modifier(
                    guild_id=guild_id,
                    modifier_id=gm_cfg.get("id", ""),
                    duration_seconds=int(gm_cfg.get("duration_seconds", 0)),
                    payload=gm_cfg.get("payload") or {},
                )
                guild_modifier_set = dict(gm_cfg)
            except Exception:
                logger.debug("guild_modifier_on_success set failed", exc_info=True)

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
                protection_service=getattr(self, "protection_service", None),
                event_key_prefix=(
                    f"dig-event:{guild_id}:{discord_id}:{event_id}:{uuid.uuid4().hex}"
                ),
            )

        # A burn-on-success event only pays out in proportion to the JC it
        # actually destroyed: when the targeted players are broke or the pool
        # is empty the burn fizzles, so the payout scales toward zero. Keeps
        # the event net-deflationary instead of minting coin it never removed.
        if (
            splash_result is not None
            and splash_cfg.get("mode", "burn") == "burn"
            and jc > 0
        ):
            nominal_burn = int(splash_cfg.get("victim_count", 0)) * int(
                scale_deflationary_minigame_jc_delta(
                    strengthen_dig_event_penalty(
                        splash_cfg.get("penalty_jc", 0)
                    )
                )
            )
            burn_ratio = (
                min(1.0, splash_result.total_burned / nominal_burn)
                if nominal_burn > 0
                else 0.0
            )
            if burn_ratio < 1.0:
                logger.info(
                    "Dig burn-event %r payout scaled to %.2f (burned %d/%d)",
                    event_id, burn_ratio, splash_result.total_burned, nominal_burn,
                )
            jc = int(round(jc * burn_ratio))

        jc = (
            scale_deflationary_minigame_jc_delta(jc)
            if jc < 0
            else scale_minigame_jc_delta(jc)
        )
        if jc > 0:
            jc = self._apply_daily_economy_reward(guild_id, jc)

        # Depth shift + JC credit/debit + optional buff + audit log commit
        # together, so the actor can't be paid without the depth/buff
        # applied (or vice versa).
        gear_id = self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=jc,
            tunnel_updates=tunnel_updates or None,
            add_gear=(
                {
                    "slot": gear_definition.slot.value,
                    "tier": gear_definition.reference_tier,
                    "durability": gear_definition.max_durability,
                    "source": f"event:{event_id}",
                    "item_id": gear_definition.item_id,
                }
                if gear_definition else None
            ),
            log_detail={
                "event_id": event_id, "choice": choice, "succeeded": succeeded,
                "advance": advance, "jc": jc, "cave_in": cave_in,
                "gear": gear_definition.item_id if gear_definition else None,
                "streak_days_lost": streak_days_lost or None,
                "curse": curse_applied.get("name") if curse_applied else None,
                "splash_victims": (
                    [{"id": vid, "amount": amt} for vid, amt in splash_result.victims]
                    if splash_result else None
                ),
            },
            log_action_type="event",
        )

        # JC threat — a failed bargain/theft outcome carries a real negative
        # jc and is NOT floored, so it can push the actor into debt. Surface
        # the resulting balance when it lands negative so the embed can say
        # so plainly ("fail loud").
        balance_after = None
        if jc < 0:
            balance_after = self.player_repo.get_balance(discord_id, guild_id)

        # Check for event chaining (P7+ random; deterministic via next_event_id when player meets target's min_prestige)
        chain_event = self._chain_event(
            new_depth, prestige_level,
            event.get("rarity", "common"),
            trigger_event_id=event_id,
        )

        # Quest progression: a successful *desperate* choice on a quest-tagged
        # event advances the player's active arc. If this resolves the final
        # stage, the quest service runs the finale handler (relic grant or
        # JC + guild modifier window) inline.
        quest_finale = None
        quest_service = getattr(self, "quest_service", None)
        if (
            event.get("quest_id")
            and choice == "desperate"
            and succeeded
            and quest_service is not None
        ):
            try:
                quest_finale = quest_service.advance_on_desperate_success(
                    discord_id, guild_id, event_id,
                )
            except Exception:
                logger.exception("quest advance_on_desperate_success failed")

        return self._ok(
            event_name=event.get("name", "Unknown Event"),
            choice=choice,
            succeeded=succeeded,
            jc_delta=jc,
            depth_delta=advance,
            cave_in=cave_in,
            streak_loss=streak_days_lost,
            curse_applied=curse_applied,
            balance_after=balance_after,
            message=description,
            buff_applied=buff_applied,
            chain_event=chain_event,
            boss_encounter=boss_encounter,
            boss_info=boss_info,
            splash=_splash_to_dict(splash_result),
            guild_modifier_set=guild_modifier_set,
            quest_finale=quest_finale,
            black_wax_seal_spent=black_wax_seal_spent,
            gear_drop=(
                {
                    "gear_id": gear_id,
                    "item_id": gear_definition.item_id,
                    "name": gear_definition.name,
                    "slot": gear_definition.slot.value,
                    "durability": gear_definition.max_durability,
                    "max_durability": gear_definition.max_durability,
                    "effect": gear_definition.effect_summary,
                }
                if gear_definition else None
            ),
        )

    # ------------------------------------------------------------------
    # Abandon Tunnel
    # ------------------------------------------------------------------
