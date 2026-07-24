"""DigCoreMixin mixin for :class:`DigService`.

First dig, queued-item resolution, cave-in consequences, and
the DM-mode precondition/outcome helpers.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import datetime
import json
import random
import time

import services.dig_service as dig_service
from services.dig._common import (
    logger,
)
from services.dig_constants import (
    BASE_DIG_JC_PAYOUT_CAP,
    BOSS_BOUNDARIES,
    BOSS_PREP_ITEM_IDS,
    CAVE_IN_BLOCK_LOSS_RANGES,
    CAVE_IN_CATASTROPHIC_GEAR_TICKS,
    CAVE_IN_CATASTROPHIC_MEDICAL_BILL,
    CAVE_IN_CATASTROPHIC_MILESTONE_STEP,
    CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE,
    CAVE_IN_INJURY_DIGS_BY_BAND,
    CAVE_IN_MEDICAL_BILL_RANGES,
    CAVE_IN_STUN_DIGS_BY_BAND,
    DIG_POSITIVE_JC_MULTIPLIER,
    DIG_STREAK_JC_PAYOUT_CAP,
    FREE_DIG_COOLDOWN,
    INJURY_SLOW_COOLDOWN,
    MILESTONES,
    STREAKS,
    cave_in_band,
    pick_cave_in_consequence,
    roll_catastrophic_cave_in,
    scale_positive_dig_jc,
)
from utils.economy_scaling import (
    scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
)


class DigCoreMixin:
    """DigCoreMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    def _get_free_dig_cooldown_duration(self, tunnel: dict, *, mana_effects=None) -> int:
        """Return the effective free-dig cooldown after every modifier."""
        cooldown = FREE_DIG_COOLDOWN
        # Mutation: restless — extra cooldown.
        mutations = self._get_mutations(tunnel)
        mutation_fx = self._apply_mutation_effects(mutations)
        cooldown += int(mutation_fx.get("cooldown_bonus_seconds", 0))
        # A slower-cooldown injury overrides the base and mutation bonus.
        injury = None
        if tunnel.get("injury_state"):
            try:
                injury = json.loads(tunnel["injury_state"])
            except (json.JSONDecodeError, TypeError):
                injury = None
        if injury and injury.get("type") == "slower_cooldown":
            cooldown = INJURY_SLOW_COOLDOWN
        cooldown = self._apply_stamina_to_cooldown(cooldown, tunnel)
        curse = self._get_active_curse(tunnel)
        curse_effects = self._apply_curse_effects(curse)
        cooldown_penalty = self._capped_curse_effect(
            curse_effects, "cooldown_penalty",
        )
        cooldown = int(cooldown * (1.0 + cooldown_penalty))
        if mana_effects is None:
            return self._apply_mana_cooldown_reduction(
                tunnel.get("discord_id"), tunnel.get("guild_id"), cooldown,
            )
        return self._apply_mana_cooldown_reduction(
            tunnel.get("discord_id"), tunnel.get("guild_id"), cooldown,
            effects=mana_effects,
        )

    def _get_cooldown_remaining(
        self,
        tunnel: dict,
        *,
        now: int | None = None,
        mana_effects=None,
    ) -> int:
        """Return seconds remaining on the effective free-dig cooldown."""
        if tunnel.get("last_dig_at") is None:
            return 0
        current_time = int(time.time()) if now is None else int(now)
        elapsed = current_time - int(tunnel["last_dig_at"])
        return max(
            0,
            self._get_free_dig_cooldown_duration(
                tunnel, mana_effects=mana_effects,
            )
            - elapsed,
        )

    def get_free_dig_ready_at(
        self, discord_id: int, guild_id, *, now: int | None = None,
    ) -> int | None:
        """Return the effective ready timestamp, or ``None`` when already ready."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        return self._get_free_dig_ready_at_from_tunnel(
            tunnel,
            discord_id,
            guild_id,
            now=now,
        )

    def get_free_dig_ready_times_bulk(
        self,
        discord_ids: list[int],
        guild_id,
        *,
        now: int | None = None,
    ) -> dict[int, int | None]:
        """Resolve restart reminder times from one tunnel and mana snapshot."""
        unique_ids = list(dict.fromkeys(discord_ids))
        if not unique_ids:
            return {}

        requested_ids = set(unique_ids)
        tunnels = {
            tunnel["discord_id"]: tunnel
            for tunnel in self.dig_repo.get_all_tunnels(guild_id)
            if tunnel["discord_id"] in requested_ids
        }
        mana_effects = {}
        if self.mana_effects_service is not None:
            try:
                mana_effects = self.mana_effects_service.get_effects_bulk(
                    unique_ids,
                    guild_id,
                )
            except Exception:
                logger.exception("Failed to bulk-load mana effects for dig reminder recovery")

        ready_times: dict[int, int | None] = {}
        for discord_id in unique_ids:
            try:
                ready_times[discord_id] = self._get_free_dig_ready_at_from_tunnel(
                    tunnels.get(discord_id),
                    discord_id,
                    guild_id,
                    now=now,
                    mana_effects=mana_effects.get(discord_id),
                )
            except Exception:
                logger.exception(
                    "Failed to calculate dig reminder for discord_id=%d guild_id=%s",
                    discord_id,
                    guild_id,
                )
        return ready_times

    def _get_free_dig_ready_at_from_tunnel(
        self,
        tunnel: dict | None,
        discord_id: int,
        guild_id,
        *,
        now: int | None = None,
        mana_effects=None,
    ) -> int | None:
        if tunnel is None or tunnel.get("last_dig_at") is None:
            return None
        tunnel = dict(tunnel)
        tunnel["discord_id"] = discord_id
        tunnel["guild_id"] = guild_id
        ready_at = int(tunnel["last_dig_at"]) + self._get_free_dig_cooldown_duration(
            tunnel,
            mana_effects=mana_effects,
        )
        current_time = int(time.time()) if now is None else int(now)
        return ready_at if ready_at > current_time else None

    def _apply_blood_pact_skim_to_payout(
        self, discord_id: int, guild_id, jc_earned: int
    ) -> int:
        """Skim an active Blood Pact's share off a dig payout.

        Returns the JC the digger nets after the skim; the pact holder receives
        the skimmed amount via an atomic, self-rolling-back transfer. No-op when
        there is no buff service or nothing to skim. Shared by every live and
        DM-mode dig payout path so the skim behaviour can't drift between them.
        """
        if self.buff_service is None or jc_earned <= 0:
            return jc_earned
        try:
            skimmed = self.buff_service.apply_blood_pact_skim(
                discord_id, guild_id, jc_earned, self.player_repo
            )
            if skimmed:
                return max(0, jc_earned - skimmed)
        except Exception:
            logger.exception("Failed to apply Blood Pact skim to dig payout")
        return jc_earned

    def _is_unstarted_tunnel(self, tunnel: dict) -> bool:
        """True for profile-created tunnels that have not had a first dig yet."""
        return (
            (tunnel.get("total_digs", 0) or 0) == 0
            and tunnel.get("last_dig_at") is None
            and (tunnel.get("depth", 0) or 0) == 0
        )

    # ── Layer Weather ────────────────────────────────────────────────

    def _build_parked_boss_return(
        self, tunnel: dict, discord_id: int, guild_id
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
            is_first_dig=False,
            dig_consumed=False,
            items_used=[],
            items_used_ids=[],
            pickaxe_tier=self._get_active_pickaxe_tier(discord_id, guild_id, tunnel),
            tip="A boss blocks your path!",
            luminosity_info=None,
        )

    def _execute_first_dig(
        self, discord_id: int, guild_id, tunnel: dict, depth_before: int, now: int, today: str
    ) -> dict:
        """Run the first-ever dig for a tunnel: guaranteed safe, writes the
        initial depth/streak/run counters, awards small JC, returns a welcome
        result dict."""
        advance = random.randint(3, 7)
        jc_earned = random.randint(1, 5)
        jc_earned = scale_minigame_jc_delta(jc_earned)
        jc_earned = self._apply_daily_economy_reward(guild_id, jc_earned)
        gross_jc = jc_earned
        jc_earned = scale_positive_dig_jc(gross_jc)
        new_depth = depth_before + advance

        # Tunnel advance + JC payout commit together so a crash can't
        # leave the depth written with the player unpaid.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=jc_earned,
            tunnel_updates={
                "depth": new_depth,
                "total_digs": (tunnel.get("total_digs", 0) or 0) + 1,
                "last_dig_at": now,
                "total_jc_earned": (tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
                "streak_days": 1,
                "streak_last_date": today,
            },
        )
        # Blood Pact skims the first dig's payout too (tiny amount, but coverage
        # must be uniform across every dig path).
        jc_earned = self._apply_blood_pact_skim_to_payout(discord_id, guild_id, jc_earned)
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="dig",
            depth_before=depth_before,
            depth_after=new_depth,
            jc_delta=jc_earned,
            details=json.dumps({
                "advance": advance, "jc": jc_earned, "first_dig": True,
                "gross_jc": gross_jc,
                "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
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
            is_first_dig=True,
            dig_consumed=False,
            items_used=[],
            items_used_ids=[],
            pickaxe_tier=0,
            tip="Welcome to the mines! Use /dig again after the cooldown.",
        )

    def _consume_streak_charm(self, discord_id: int, guild_id) -> bool:
        """Consume one passive Streak Charm if the player has one."""
        inventory = self.dig_repo.get_inventory(discord_id, guild_id) or []
        if not any(i.get("item_type") == "streak_charm" for i in inventory):
            return False
        self.dig_repo.remove_inventory_item(discord_id, guild_id, "streak_charm")
        return True

    def _calculate_daily_streak(
        self, discord_id: int, guild_id, tunnel: dict, today: str
    ) -> tuple[int, bool]:
        """Return today's streak value and whether a Streak Charm was consumed."""
        streak = tunnel.get("streak_days", 0) or 0
        streak_last = tunnel.get("streak_last_date")
        today_dt = datetime.datetime.strptime(today, "%Y-%m-%d")
        yesterday = (today_dt - datetime.timedelta(days=1)).strftime("%Y-%m-%d")
        grace_day = (today_dt - datetime.timedelta(days=2)).strftime("%Y-%m-%d")

        if streak_last == yesterday:
            return streak + 1, False
        if streak_last == today:
            return streak, False
        if (
            streak_last == grace_day
            and streak > 0
            and self._consume_streak_charm(discord_id, guild_id)
        ):
            return streak + 1, True
        return 1, False

    def _resolve_queued_items(
        self, discord_id: int, guild_id
    ) -> tuple[list[str], list[str], dict[str, bool], list[int]]:
        """Read queued items and return display names, item-type ids, a flag-map
        (one ``has_<item>`` key per consumable), and the inventory ROW ids to
        consume.

        This method only READS — it does NOT delete the rows. The caller must
        fold the returned row ids into its final atomic commit (via
        ``atomic_tunnel_balance_update(consume_inventory_item_ids=...)``) so the
        item burn commits-or-rolls-back together with the dig result. An earlier
        version deleted here, which permanently destroyed consumables if the dig
        raised before its commit."""
        queued = self._get_queued_items_for_tunnel(discord_id, guild_id)
        items_used: list[str] = []
        items_used_ids: list[str] = []
        consumed_row_ids: list[int] = []
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
            if itype in BOSS_PREP_ITEM_IDS:
                continue
            if itype in _display_names:
                items_used.append(_display_names[itype])
            flag_key = f"has_{itype}" if itype else None
            if flag_key and flag_key in flags:
                flags[flag_key] = True
            if itype:
                items_used_ids.append(itype)
            row_id = item.get("id")
            if row_id is not None:
                consumed_row_ids.append(int(row_id))

        return items_used, items_used_ids, flags, consumed_row_ids

    def _apply_cave_in_consequence(
        self,
        *,
        discord_id: int,
        guild_id,
        tunnel: dict,
        depth_before: int,
        band: str,
        block_loss: int,
        block_loss_cap: int | None = None,
        catastrophic: bool,
        balance: int,
        injury_bonus: int,
        tunnel_updates: dict,
    ) -> tuple[dict, int]:
        """Pick & apply a cave-in consequence (or catastrophic upgrade).

        Mutates ``tunnel_updates`` with any tunnel-state changes (the caller
        is responsible for writing them atomically). Side
        effects that aren't tunnel-state (gear durability tick, inventory
        item removal) are applied to repos directly. Returns
        ``(cave_in_detail, jc_debit)`` where ``jc_debit`` is the JC amount
        the caller should subtract from the player's balance.
        """
        def gear_name_map(equipped_gear: dict) -> dict[int, str]:
            names: dict[int, str] = {}
            for row in equipped_gear.values():
                piece = self._hydrate_gear_piece(row)
                if piece is not None:
                    names[piece.id] = piece.tier_def.name
            return names

        # Catastrophic overrides the consequence pick entirely.
        if catastrophic:
            # Insurance keeps the player at depth — covers the rollback only,
            # not the other catastrophic effects (medical bill, stun, gear).
            now_ts = int(time.time())
            insured = int(tunnel.get("insured_until") or 0) > now_ts
            insurance_saved = False
            if insured:
                # Skip the rollback; depth stays at the block_loss-driven value
                # already in tunnel_updates.
                new_depth = int(tunnel_updates.get("depth", depth_before - block_loss))
                insurance_saved = True
            else:
                # Catastrophic overrides block_loss with a hard roll-back to
                # the nearest 25-multiple milestone strictly less than
                # depth_before.
                milestone = max(
                    0,
                    ((max(0, depth_before - 1)) // CAVE_IN_CATASTROPHIC_MILESTONE_STEP)
                    * CAVE_IN_CATASTROPHIC_MILESTONE_STEP,
                )
                capped_depth = (
                    depth_before - max(0, int(block_loss_cap))
                    if block_loss_cap is not None
                    else milestone
                )
                new_depth = max(milestone, capped_depth)
                tunnel_updates["depth"] = new_depth
            tunnel_updates["temp_buffs"] = None  # nukes any second_wind set above

            cmin, cmax = CAVE_IN_CATASTROPHIC_MEDICAL_BILL
            med_cost = min(random.randint(cmin, cmax), max(0, balance))

            smin, smax = CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE
            stun_digs = random.randint(smin, smax) + injury_bonus
            tunnel_updates["injury_state"] = json.dumps(
                {"type": "slower_cooldown", "digs_remaining": stun_digs}
            )

            try:
                equipped = self.dig_repo.get_equipped_gear(discord_id, guild_id) or {}
            except Exception:
                equipped = {}
            name_by_id = gear_name_map(equipped)
            broken_ids: list[int] = []
            for _ in range(CAVE_IN_CATASTROPHIC_GEAR_TICKS):
                try:
                    broken_ids.extend(
                        self.dig_repo.tick_gear_durability(discord_id, guild_id)
                    )
                except Exception:
                    logger.debug("catastrophic gear tick failed", exc_info=True)
                    break
            gear_broken = [
                name_by_id.get(gear_id, "a piece of gear")
                for gear_id in dict.fromkeys(broken_ids)
            ]

            total_block_loss = max(block_loss, depth_before - new_depth)
            insurance_note = " Insurance held the depth." if insurance_saved else ""
            detail = {
                "type": "catastrophic",
                "block_loss": total_block_loss,
                "jc_lost": med_cost,
                "stun_digs": stun_digs,
                "depth_after": new_depth,
                "insurance_saved": insurance_saved,
                "gear_broken": gear_broken,
                "message": (
                    f"CATASTROPHIC CAVE-IN! Tunnel folds in on itself. "
                    f"Lost {total_block_loss} blocks, paid {med_cost} JC, "
                    f"stunned for {stun_digs} digs, gear shattered."
                    f"{insurance_note}"
                ),
            }
            return detail, med_cost

        # Non-catastrophic: weighted pick based on current state.
        try:
            inventory = self.dig_repo.get_inventory(discord_id, guild_id) or []
        except Exception:
            inventory = []
        try:
            equipped = self.dig_repo.get_equipped_gear(discord_id, guild_id) or {}
        except Exception:
            equipped = {}
        luminosity_now = int(tunnel.get("luminosity") or 0)
        hard_hat_charges = int(tunnel.get("hard_hat_charges") or 0)

        consequence_id = pick_cave_in_consequence(
            band,
            has_consumables=bool(inventory),
            has_equipped_gear=any(
                int(row.get("durability") or 0) > 0
                for row in equipped.values()
            ),
            can_lower_luminosity=luminosity_now > 0,
            has_hard_hat_charges=hard_hat_charges > 0,
        )

        if consequence_id == "stun":
            stun_digs = CAVE_IN_STUN_DIGS_BY_BAND[band] + injury_bonus
            tunnel_updates["injury_state"] = json.dumps(
                {"type": "slower_cooldown", "digs_remaining": stun_digs}
            )
            return (
                {
                    "type": "stun", "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're stunned.",
                },
                0,
            )
        if consequence_id == "injury":
            injury_digs = CAVE_IN_INJURY_DIGS_BY_BAND[band] + injury_bonus
            tunnel_updates["injury_state"] = json.dumps(
                {"type": "reduced_advance", "digs_remaining": injury_digs}
            )
            return (
                {
                    "type": "injury", "block_loss": block_loss,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks and you're injured "
                        f"(reduced digging for {injury_digs} digs)."
                    ),
                },
                0,
            )
        if consequence_id == "medical_bill":
            bmin, bmax = CAVE_IN_MEDICAL_BILL_RANGES[band]
            med_cost = min(random.randint(bmin, bmax), max(0, balance))
            return (
                {
                    "type": "medical_bill", "block_loss": block_loss,
                    "jc_lost": med_cost,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks and paid "
                        f"{med_cost} JC in medical bills."
                    ),
                },
                med_cost,
            )
        if consequence_id == "gear_nick":
            name_by_id = gear_name_map(equipped)
            broken_ids = []
            try:
                broken_ids = self.dig_repo.tick_gear_durability(discord_id, guild_id)
            except Exception:
                logger.debug("gear_nick tick failed", exc_info=True)
            return (
                {
                    "type": "gear_nick", "block_loss": block_loss,
                    "gear_broken": [
                        name_by_id.get(gear_id, "a piece of gear")
                        for gear_id in broken_ids
                    ],
                    "message": f"Cave-in! Lost {block_loss} blocks. Gear took a beating.",
                },
                0,
            )
        if consequence_id == "spilled_satchel" and inventory:
            item = random.choice(inventory)
            item_type = item.get("type") or item.get("item_type") or ""
            item_name = item.get("name") or item_type or "an item"
            if item_type:
                try:
                    self.dig_repo.remove_inventory_item(discord_id, guild_id, item_type)
                except Exception:
                    logger.debug("spilled_satchel removal failed", exc_info=True)
            return (
                {
                    "type": "spilled_satchel", "block_loss": block_loss,
                    "item_lost": item_name,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks. "
                        f"Your {item_name} spills into the dark."
                    ),
                },
                0,
            )
        if consequence_id == "snuffed_light" and luminosity_now > 0:
            new_lum = max(0, luminosity_now - 25)
            tunnel_updates["luminosity"] = new_lum
            return (
                {
                    "type": "snuffed_light", "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks. The dark presses in.",
                },
                0,
            )
        if consequence_id == "cracked_hat" and hard_hat_charges > 0:
            tunnel_updates["hard_hat_charges"] = max(0, hard_hat_charges - 1)
            return (
                {
                    "type": "cracked_hat", "block_loss": block_loss,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks. "
                        f"Your hard hat takes a chunk out of itself."
                    ),
                },
                0,
            )

        # Fallback: medical bill if the picker landed on something inapplicable.
        bmin, bmax = CAVE_IN_MEDICAL_BILL_RANGES[band]
        med_cost = min(random.randint(bmin, bmax), max(0, balance))
        return (
            {
                "type": "medical_bill", "block_loss": block_loss,
                "jc_lost": med_cost,
                "message": (
                    f"Cave-in! Lost {block_loss} blocks and paid "
                    f"{med_cost} JC in medical bills."
                ),
            },
            med_cost,
        )

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

        # Cave-in check. Hard Hat absorb also drains luminosity (matches the
        # main dig() flow).
        luminosity_after_hh = p["luminosity"]
        if p["hard_hat_prevents"]:
            cave_in = False
            luminosity_after_hh = max(0, luminosity_after_hh - 10)
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                hard_hat_charges=p["hard_hat_charges"] - 1,
                luminosity=luminosity_after_hh,
            )
            p["lum_info"]["luminosity_after"] = luminosity_after_hh
            p["lum_info"]["drained"] = p["lum_info"].get("drained", 0) + 10
        else:
            cave_in = random.random() < p["cave_in_chance"]
        cave_in_detail = None

        if cave_in:
            band = cave_in_band(depth_before)
            block_min, block_max = CAVE_IN_BLOCK_LOSS_RANGES[band]
            block_loss = random.randint(block_min, block_max)
            weather_loss_cap = p["weather_fx"].get("cave_in_loss_cap")
            block_loss += int(p["weather_fx"].get("cave_in_loss_bonus", 0))
            block_loss += int(p["mutation_fx"].get("cave_in_loss_bonus", 0))
            block_loss = self._apply_route_cave_in_loss(
                block_loss,
                p.get("route_effects", {}),
                weather_loss_cap,
            )
            steady_hands_reduction = p.get("perk_fx", {}).get("cave_in_loss_reduction", 0.0)
            if steady_hands_reduction > 0:
                block_loss = max(0, int(block_loss * (1.0 - steady_hands_reduction)))
            # Relic: Patient Stone — -30% depth lost
            if self._has_relic(discord_id, guild_id, "patient_stone"):
                block_loss = max(0, int(block_loss * 0.7))
            # Reinforcement remains the final loss cap after player reductions.
            reinforced_until_for_cap = tunnel.get("reinforced_until") or 0
            reinforcement_loss_cap = (
                8 if now < int(reinforced_until_for_cap) else None
            )
            if reinforcement_loss_cap is not None:
                block_loss = min(block_loss, 8)
            catastrophic_loss_cap = self._effective_cave_in_loss_cap(
                p.get("route_effects", {}),
                weather_loss_cap,
                reinforcement_loss_cap,
            )
            block_loss_pre_save = block_loss
            grappling_hook_charges = int(p.get("grappling_hook_charges") or 0)
            grappling_absorbed = False
            if grappling_hook_charges > 0:
                block_loss = 0
                grappling_absorbed = True
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    grappling_hook_charges=grappling_hook_charges - 1,
                )
            elif p["pickaxe_tier"] >= 7:
                block_loss = max(1, block_loss - 1)
            new_depth = max(0, depth_before - block_loss)
            # Relic: Gambler's Charm — bonus JC equal to 50% of would-have-lost depth
            gamblers_charm_bonus = 0
            if (
                block_loss_pre_save > 0
                and self._has_relic(discord_id, guild_id, "gamblers_charm")
            ):
                gamblers_charm_bonus = max(1, int(block_loss_pre_save * 0.5))

            tunnel_updates: dict = {
                "depth": new_depth,
                "total_digs": (tunnel.get("total_digs", 0) or 0) + 1,
                "last_dig_at": now,
                "cavein_free_streak": 0,  # Prospector's Streak resets on collapse
            }

            if p["thick_skin_saved"]:
                tunnel_updates["thick_skin_date"] = today

            cave_in_jc = 0
            loot_chance = p["mutation_fx"].get("cave_in_loot_chance", 0)
            if loot_chance > 0 and random.random() < loot_chance:
                loot_min = int(p["mutation_fx"].get("cave_in_loot_min", 1))
                loot_max = int(p["mutation_fx"].get("cave_in_loot_max", 3))
                cave_in_jc = random.randint(loot_min, loot_max)
            if gamblers_charm_bonus > 0:
                cave_in_jc += gamblers_charm_bonus
            cave_in_gross_jc = cave_in_jc
            cave_in_jc = scale_positive_dig_jc(cave_in_gross_jc)

            if p["mutation_fx"].get("post_cave_in_advance"):
                tunnel_updates["temp_buffs"] = json.dumps({
                    "id": "second_wind", "name": "Second Wind",
                    "digs_remaining": 1,
                    "effect": {"advance_bonus": int(p["mutation_fx"]["post_cave_in_advance"])},
                })

            injury_bonus = int(p["mutation_fx"].get("injury_duration_bonus", 0))
            balance = self.player_repo.get_balance(discord_id, guild_id)
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
                    block_loss_cap=catastrophic_loss_cap,
                    catastrophic=catastrophic,
                    balance=balance,
                    injury_bonus=injury_bonus,
                    tunnel_updates=tunnel_updates,
                )
            # Net JC change (loot credit minus consequence debit) and the
            # tunnel write commit together so a crash can't leave depth lost
            # without the matching balance change.
            net_delta = cave_in_jc - jc_debit
            if catastrophic:
                new_depth = tunnel_updates["depth"]

            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=net_delta,
                tunnel_updates=tunnel_updates,
                consume_inventory_item_ids=p.get("consumed_item_row_ids") or [],
            )
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
                details=json.dumps({
                    "cave_in": True, "block_loss": block_loss,
                    "detail": cave_in_detail,
                    "depth_before": depth_before, "depth_after": new_depth,
                    "gross_jc": cave_in_gross_jc,
                    "reward_multiplier": (
                        DIG_POSITIVE_JC_MULTIPLIER if cave_in_gross_jc > 0 else None
                    ),
                }),
            )
            if p.get("overgrowth_active") and self.buff_service is not None:
                try:
                    self.buff_service.consume_overgrowth_charge(discord_id, guild_id)
                except Exception:
                    logger.exception("Failed to consume Overgrowth charge")
            return self._ok(
                tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                depth_before=depth_before, depth_after=new_depth,
                advance=0, jc_earned=0, milestone_bonus=0, streak_bonus=0,
                cave_in=True, cave_in_detail=cave_in_detail,
                boss_encounter=False, boss_info=None,
                has_lantern=p["has_lantern"],
                event=None, artifact=None,
                is_first_dig=False,
                dig_consumed=True,
                items_used=p["items_used"], items_used_ids=p["items_used_ids"],
                auto_purchases=p.get("auto_purchases", []),
                pickaxe_tier=p["pickaxe_tier"],
                tip=self._pick_tip(new_depth),
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
        base_max = max(
            base_min,
            base_max
            - int(p.get("route_effects", {}).get("advance_max_penalty", 0)),
        )
        base_max = max(base_min, base_max - int(p["mutation_fx"].get("advance_max_penalty", 0)))
        if p["corruption"] and p["corruption"]["effects"].get("min_advance_roll"):
            roll1 = random.randint(base_min, base_max)
            roll2 = random.randint(base_min, base_max)
            advance = min(roll1, roll2)
        else:
            advance = random.randint(base_min, base_max)

        advance += p["pickaxe_advance_bonus"] + p["mole_claws_bonus"] + p["buff_advance_bonus"]
        advance += int(p.get("route_effects", {}).get("advance_bonus", 0))
        # Relic: Pathfinder's Spur — +1 advance in the deep layers (depth 150+).
        if depth_before >= 150 and self._has_relic(discord_id, guild_id, "pathfinders_spur"):
            advance += 1
        # Temp-curse advance modifier (negative = slowed dig)
        advance += int(p.get("curse_advance_bonus", 0))
        advance += int(p["weather_fx"].get("advance_bonus", 0))
        advance -= int(p["ascension"].get("advance_penalty", 0))
        if p["corruption"]:
            advance -= int(p["corruption"]["effects"].get("advance_penalty", 0))
        dynamite_bonus = 5 if p["has_dynamite"] else 0
        depth_charge_bonus = 10 if p["has_depth_charge"] else 0
        advance = int(
            advance * (1.0 + p["perk_advance_bonus"]) * p["injury_advance_mod"]
        )
        advance += int(p.get("perk_advance_flat", 0) + 0.5)
        # Consumable bonuses are flat - applied after the multiplier so an
        # injury or negative perk can't shrink the advertised +5 / +10.
        advance += dynamite_bonus + depth_charge_bonus
        advance = max(1, advance)
        advance = min(
            advance,
            self._get_dig_advance_cap(
                stat_effects,
                has_dynamite=p["has_dynamite"],
                has_depth_charge=p["has_depth_charge"],
            ),
        )

        # Boss boundary
        boss_progress = self._get_boss_progress(tunnel)
        next_boss = self._next_boss_boundary(boss_progress)
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
        jc_mult = max(0.0, jc_mult - p["ascension"].get("jc_layer_penalty", 0))
        weather_code_now = self._get_weather_code(guild_id, layer_name)
        relic_yield_mult = self._relic_jc_yield_multiplier(
            discord_id, guild_id,
            weather_code=weather_code_now,
            luminosity=luminosity,
            is_first_dig_today=self._is_first_dig_of_day(tunnel.get("last_dig_at"), p["today"]),
            is_paid_dig=p.get("paid_dig_cost", 0) > 0,
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
        jc_earned = (
            int(
                jc_earned
                * jc_mult
                * relic_yield_mult
                * weather_combo_yield
                * self._luminosity_jc_multiplier(luminosity)
                * self._post_pinnacle_decay_factor(new_depth, discord_id, guild_id)
            )
            + p["magma_heart_bonus"]
            + int(p.get("perk_loot_flat", 0) + 0.5)
        )
        jc_earned += int(p["weather_fx"].get("jc_bonus", 0))
        # Temp-curse JC drain (negative jc_bonus = less JC this dig). The
        # max(0, ...) clamp below still floors a cursed dig at 0.
        jc_earned += int(p.get("curse_jc_bonus", 0))
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

        # Mana variance + steady bonus on base loot only.
        jc_earned = self._apply_mana_yield_variance(discord_id, guild_id, jc_earned)

        # Relic: Prospector's Streak — flat JC per consecutive cave-in-free dig
        # (capped). Folded into the non-streak total so it counts toward the base
        # cap instead of stacking past it. The counter is bumped here and
        # persisted below; the cave-in branch resets it to 0.
        cavein_free_streak = (tunnel.get("cavein_free_streak", 0) or 0) + 1
        if self._has_relic(discord_id, guild_id, "prospectors_streak"):
            jc_earned += min(cavein_free_streak, 20)

        # Cap the non-streak payout (base loot + relic). Milestones and the
        # daily-streak bonus are separate buckets added on top.
        jc_earned = min(jc_earned, BASE_DIG_JC_PAYOUT_CAP)
        nonstreak_jc = jc_earned  # pre-tax capped basis for the flavor clamp

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
        streak, streak_charm_used = self._calculate_daily_streak(
            discord_id, guild_id, tunnel, today
        )
        streak_bonus = 0
        for threshold in sorted(STREAKS.keys(), reverse=True):
            if streak >= threshold:
                streak_bonus = STREAKS[threshold]
                break
        # Perk: patient_step boosts streak JC
        streak_bonus = int(
            streak_bonus * (1.0 + p.get("perk_fx", {}).get("streak_bonus_multiplier", 0.0))
        )
        streak_bonus = min(streak_bonus, DIG_STREAK_JC_PAYOUT_CAP)
        jc_earned += streak_bonus

        jc_earned = scale_minigame_jc_delta(jc_earned)
        gross_jc = jc_earned
        jc_earned = scale_positive_dig_jc(gross_jc)
        overgrowth_bonus = 10 if p.get("overgrowth_active") else 0
        jc_earned += overgrowth_bonus

        # Plains tithe / Blue tax apply to the scaled payout plus Overgrowth.
        # (_apply_mana_yield_taxes also applies the daily economy event.)
        jc_earned = self._apply_mana_yield_taxes(discord_id, guild_id, jc_earned)
        # Helltide bell: flat per-dig tax while the guild modifier is active.
        helltide_tax = scale_deflationary_minigame_jc_delta(self._helltide_tax(guild_id))
        if helltide_tax > 0:
            jc_earned = max(0, jc_earned - helltide_tax)

        # Artifact
        artifact = None
        if not (p["corruption"] and p["corruption"]["effects"].get("skip_artifact")):
            artifact = self.roll_artifact(
                discord_id, guild_id, new_depth,
                extra_rate_mod=(
                    p["weather_fx"].get("artifact_multiplier", 1.0)
                    * p.get("route_effects", {}).get("artifact_multiplier", 1.0)
                ),
            )

        # Event
        void_bait_digs = tunnel.get("void_bait_digs", 0) or 0
        void_bait_charge_used = void_bait_digs > 0
        if void_bait_charge_used:
            self.dig_repo.update_tunnel(
                discord_id, guild_id, void_bait_digs=void_bait_digs - 1,
            )
        event = None
        sonar_skip_consumed = False
        sonar_skip_active_this_dig = bool(p.get("sonar_skip_active_this_dig"))
        event_preview_skipped = None
        if random.random() < p["event_chance"]:
            event = self.roll_event(
                new_depth, luminosity=luminosity, prestige_level=p["prestige_level"],
                discord_id=discord_id, guild_id=guild_id, in_boss=boss_encounter,
                tunnel=tunnel, void_bait_active=void_bait_charge_used,
            )
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
                event["hollow_eye_revealed"] = True

        event_preview = None
        boss_scout = None
        if p["has_lantern"] or p["has_sonar_pulse"]:
            preview = self.roll_event(
                new_depth, luminosity=luminosity, prestige_level=p["prestige_level"],
                discord_id=discord_id, guild_id=guild_id, in_boss=boss_encounter,
                tunnel=tunnel,
            )
            if preview:
                event_preview = {
                    "name": preview.get("name"),
                    "description": preview.get("description"),
                    "rarity": preview.get("rarity", "common"),
                }
        if p["has_lantern"]:
            for boundary in BOSS_BOUNDARIES:
                if boundary > new_depth and boundary - new_depth <= 10:
                    boss_scout = {
                        "blocks_until": boundary - new_depth,
                        "depth": boundary,
                    }
                    break
        if sonar_skip_consumed and event_preview is None:
            event_preview = event_preview_skipped
        if sonar_skip_consumed:
            self.dig_repo.update_tunnel(
                discord_id, guild_id, sonar_skip_pending=0,
            )

        total_digs = (tunnel.get("total_digs", 0) or 0) + 1

        # Bankruptcy debuff: keep only the configured fraction of yield while
        # penalized (applied last, before the credit; withheld share is a sink).
        jc_earned, dig_bankruptcy_penalty = self._penalize_jc(discord_id, guild_id, jc_earned)

        # DB writes
        run_jc = (tunnel.get("current_run_jc", 0) or 0) + jc_earned
        run_artifacts = (tunnel.get("current_run_artifacts", 0) or 0) + (1 if artifact else 0)
        run_events_count = (tunnel.get("current_run_events", 0) or 0) + (1 if event else 0)
        # Tunnel advance + JC payout commit together so a crash can't
        # leave the depth written with the player unpaid.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=jc_earned,
            tunnel_updates={
                "depth": new_depth, "total_digs": total_digs, "last_dig_at": now,
                "max_depth": max(prev_max_depth, new_depth),
                "total_jc_earned": (tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
                "streak_days": streak, "streak_last_date": today,
                "cavein_free_streak": cavein_free_streak,
                "current_run_jc": run_jc,
                "current_run_artifacts": run_artifacts,
                "current_run_events": run_events_count,
            },
            consume_inventory_item_ids=p.get("consumed_item_row_ids") or [],
        )
        jc_earned = self._apply_blood_pact_skim_to_payout(discord_id, guild_id, jc_earned)
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id, action_type="dig",
            details=json.dumps({
                "advance": advance, "jc": jc_earned,
                "gross_jc": gross_jc,
                "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
                "overgrowth_bonus": overgrowth_bonus,
                "depth_before": depth_before, "depth_after": new_depth,
                "boss_encounter": boss_encounter, "cave_in": False,
                "corruption": p["corruption"]["id"] if p["corruption"] else None,
                "streak_charm_used": streak_charm_used,
            }),
        )
        if p.get("overgrowth_active") and self.buff_service is not None:
            try:
                self.buff_service.consume_overgrowth_charge(discord_id, guild_id)
            except Exception:
                logger.exception("Failed to consume Overgrowth charge")

        paid_dig_cost = p["paid_dig_cost"]
        return self._ok(
            tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
            depth_before=depth_before, depth_after=new_depth,
            advance=advance, jc_earned=jc_earned, nonstreak_jc=nonstreak_jc,
            overgrowth_bonus=overgrowth_bonus,
            bankruptcy_penalty=dig_bankruptcy_penalty,
            milestone_bonus=milestone_bonus, streak_bonus=streak_bonus,
            cave_in=False, cave_in_detail=None,
            boss_encounter=boss_encounter, boss_info=boss_info,
            has_lantern=p["has_lantern"],
            event=event, artifact=artifact,
            is_first_dig=False,
            dig_consumed=True,
            items_used=p["items_used"], items_used_ids=p["items_used_ids"],
            auto_purchases=p.get("auto_purchases", []),
            pickaxe_tier=p["pickaxe_tier"],
            tip=self._pick_tip(new_depth),
            luminosity_info=p["lum_info"],
            paid_cost=paid_dig_cost if paid_dig_cost > 0 else 0,
            dynamite_bonus=dynamite_bonus,
            corruption=p["corruption"],
            mutations=[m.get("name") for m in p["mutations"]] if p["mutations"] else None,
            event_preview=event_preview,
            boss_scout=boss_scout,
            sonar_skipped=sonar_skip_consumed,
            weather=p["weather_info"],
            streak_charm_used=streak_charm_used,
        )

    def apply_dig_outcome(self, preconditions: dict, outcome: dict) -> dict:
        """Apply a DM-decided outcome to the database.

        *outcome* should contain keys from the ``resolve_dig`` tool call:
        advance, jc_earned, cave_in, cave_in_block_loss, cave_in_type,
        cave_in_jc_lost, event_id, narrative, tone.

        Handles boss-boundary capping, milestone/streak bonuses, and all
        DB writes.  Returns the standard result dict for the embed builder.
        """
        p = preconditions
        discord_id = p["discord_id"]
        guild_id = p["guild_id"]
        now = p["now"]
        today = p["today"]
        tunnel = p["tunnel"]
        depth_before = p["depth_before"]

        cave_in = outcome.get("cave_in", False)

        # Hard hat prevents cave-in regardless of DM decision. Also drains
        # 10 luminosity per absorb to match the main dig() flow.
        if p["hard_hat_prevents"]:
            cave_in = False
            luminosity_after_hh = max(0, int(p["luminosity"]) - 10)
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                hard_hat_charges=p["hard_hat_charges"] - 1,
                luminosity=luminosity_after_hh,
            )
            p["lum_info"]["luminosity_after"] = luminosity_after_hh
            p["lum_info"]["drained"] = p["lum_info"].get("drained", 0) + 10

        if cave_in:
            block_loss = outcome.get("cave_in_block_loss", 5)
            # All cave-in tunnel mutations and the net JC change accumulate
            # here and commit in one atomic write below, so a crash can't
            # leave depth lost without the matching injury/balance change.
            cave_in_tunnel_updates: dict = {}
            cave_in_balance_delta = 0
            # Enforce game-rule constraints
            grappling_hook_charges = int(p.get("grappling_hook_charges") or 0)
            grappling_absorbed = False
            block_loss = self._apply_route_cave_in_loss(
                block_loss,
                p.get("route_effects", {}),
            )
            if grappling_hook_charges > 0:
                block_loss = 0
                grappling_absorbed = True
                cave_in_tunnel_updates["grappling_hook_charges"] = grappling_hook_charges - 1
            elif p["pickaxe_tier"] >= 7:
                block_loss = max(1, block_loss - 1)
            weather_loss_cap = p["weather_fx"].get("cave_in_loss_cap")
            if weather_loss_cap is not None:
                block_loss = min(block_loss, int(weather_loss_cap))
            reinforced_until_for_cap = tunnel.get("reinforced_until") or 0
            if now < int(reinforced_until_for_cap):
                block_loss = min(block_loss, 8)
            new_depth = max(0, depth_before - block_loss)

            if p["thick_skin_saved"]:
                cave_in_tunnel_updates["thick_skin_date"] = today

            # Cave-in type from DM (overridden when grappling absorbs)
            cave_in_type = outcome.get("cave_in_type", "stun")
            if grappling_absorbed:
                cave_in_type = "cushioned"
            injury_bonus = int(p["mutation_fx"].get("injury_duration_bonus", 0))

            if cave_in_type == "cushioned":
                cave_in_detail = {
                    "type": "cushioned", "block_loss": 0,
                    "message": "Cave-in! Your grappling line snapped taut and absorbed the impact.",
                }
            elif cave_in_type == "stun":
                cave_in_detail = {
                    "type": "stun", "block_loss": block_loss,
                    "message": f"Cave-in! Lost {block_loss} blocks and you're stunned.",
                }
                cave_in_tunnel_updates["injury_state"] = json.dumps(
                    {"type": "slower_cooldown", "digs_remaining": 2 + injury_bonus}
                )
            elif cave_in_type == "injury":
                cave_in_detail = {
                    "type": "injury", "block_loss": block_loss,
                    "message": (
                        f"Cave-in! Lost {block_loss} blocks and you're injured "
                        f"(reduced digging for {3 + injury_bonus} digs)."
                    ),
                }
                cave_in_tunnel_updates["injury_state"] = json.dumps(
                    {"type": "reduced_advance", "digs_remaining": 3 + injury_bonus}
                )
            else:  # medical_bill
                med_cost = outcome.get("cave_in_jc_lost", 5)
                balance = self.player_repo.get_balance(discord_id, guild_id)
                med_cost = min(med_cost, max(0, balance))
                if med_cost > 0:
                    cave_in_balance_delta -= med_cost
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
                cave_in_gross_jc = random.randint(loot_min, loot_max)
                cave_in_balance_delta += scale_positive_dig_jc(
                    cave_in_gross_jc
                )
            else:
                cave_in_gross_jc = 0
            # Mutation: second_wind
            if p["mutation_fx"].get("post_cave_in_advance"):
                cave_in_tunnel_updates["temp_buffs"] = json.dumps({
                    "id": "second_wind", "name": "Second Wind",
                    "digs_remaining": 1,
                    "effect": {"advance_bonus": int(p["mutation_fx"]["post_cave_in_advance"])},
                })

            cave_in_tunnel_updates.update({
                "depth": new_depth,
                "total_digs": (tunnel.get("total_digs", 0) or 0) + 1,
                "last_dig_at": now,
                "cavein_free_streak": 0,  # Prospector's Streak resets on collapse
            })
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=cave_in_balance_delta,
                tunnel_updates=cave_in_tunnel_updates,
                consume_inventory_item_ids=p.get("consumed_item_row_ids") or [],
            )
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
                details=json.dumps({
                    "cave_in": True, "block_loss": block_loss,
                    "detail": cave_in_detail,
                    "depth_before": depth_before, "depth_after": new_depth,
                    "dm_mode": True,
                    "gross_jc": cave_in_gross_jc,
                    "reward_multiplier": (
                        DIG_POSITIVE_JC_MULTIPLIER if cave_in_gross_jc > 0 else None
                    ),
                }),
            )
            if p.get("overgrowth_active") and self.buff_service is not None:
                try:
                    self.buff_service.consume_overgrowth_charge(discord_id, guild_id)
                except Exception:
                    logger.exception("Failed to consume Overgrowth charge")
            result = self._ok(
                tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                depth_before=depth_before, depth_after=new_depth,
                advance=0, jc_earned=0, milestone_bonus=0, streak_bonus=0,
                cave_in=True, cave_in_detail=cave_in_detail,
                boss_encounter=False, boss_info=None,
                has_lantern=p["has_lantern"],
                event=None, artifact=None,
                is_first_dig=False,
                dig_consumed=True,
                items_used=p["items_used"], items_used_ids=p["items_used_ids"],
                auto_purchases=p.get("auto_purchases", []),
                pickaxe_tier=p["pickaxe_tier"],
                tip=self._pick_tip(new_depth),
                luminosity_info=p["lum_info"],
                weather=p["weather_info"],
            )
        else:
            # No cave-in — DM-decided advance + JC
            advance = min(
                outcome.get("advance", 1),
                self._get_dig_advance_cap(
                    p["stat_effects"],
                    has_dynamite=p["has_dynamite"],
                    has_depth_charge=p["has_depth_charge"],
                ),
            )

            # Boss boundary cap (DM cannot skip bosses)
            boss_progress = self._get_boss_progress(tunnel)
            next_boss = self._next_boss_boundary(boss_progress)
            boss_encounter = False
            boss_info = None
            if next_boss is not None and depth_before + advance >= next_boss:
                advance = max(0, next_boss - 1 - depth_before)
                boss_encounter = True
                boss_info = self._build_boss_info(discord_id, guild_id, tunnel, next_boss)
            new_depth = depth_before + advance

            jc_earned = outcome.get("jc_earned", 0)

            # Mana/weather combo affects base yield only, matching dig().
            weather_combo_yield = 1.0
            if self.mana_effects_service is not None:
                try:
                    _wc = self.mana_effects_service.get_weather_combo_modifiers(
                        discord_id, guild_id,
                        self._get_weather_code(guild_id, p["layer_name"]),
                    )
                    weather_combo_yield = _wc["yield_mult"]
                except Exception:
                    weather_combo_yield = 1.0
            if weather_combo_yield != 1.0:
                jc_earned = int(jc_earned * weather_combo_yield)

            # Relic: Prospector's Streak — folded into the non-streak total so it
            # counts toward the base cap instead of stacking past it. Counter is
            # bumped here and persisted below; the cave-in branch resets it to 0.
            cavein_free_streak = (tunnel.get("cavein_free_streak", 0) or 0) + 1
            if self._has_relic(discord_id, guild_id, "prospectors_streak"):
                jc_earned += min(cavein_free_streak, 20)

            # Cap the non-streak payout (base loot + relic). Milestones and the
            # daily-streak bonus are separate buckets added on top.
            jc_earned = min(jc_earned, BASE_DIG_JC_PAYOUT_CAP)
            nonstreak_jc = jc_earned  # pre-tax capped basis for the flavor clamp

            # Milestones (anti-farm: only award on depths that extend all-time high,
            # same as main dig() / _execute_deterministic_outcome).
            milestone_bonus = 0
            milestone_mult = 1.0 + p["ascension"].get("milestone_multiplier", 0)
            prev_max_depth = tunnel.get("max_depth", 0) or 0
            milestone_floor = max(depth_before, prev_max_depth)
            for m_depth, m_reward in MILESTONES.items():
                if milestone_floor < m_depth <= new_depth:
                    milestone_bonus += int(m_reward * milestone_mult)
            jc_earned += milestone_bonus

            # Streak (deterministic bookkeeping)
            streak, streak_charm_used = self._calculate_daily_streak(
                discord_id, guild_id, tunnel, today
            )
            streak_bonus = 0
            for threshold in sorted(STREAKS.keys(), reverse=True):
                if streak >= threshold:
                    streak_bonus = STREAKS[threshold]
                    break
            streak_bonus = min(streak_bonus, DIG_STREAK_JC_PAYOUT_CAP)
            jc_earned += streak_bonus

            # Artifact (deterministic)
            artifact = None
            if not (p["corruption"] and p["corruption"]["effects"].get("skip_artifact")):
                artifact = self.roll_artifact(
                    discord_id, guild_id, new_depth,
                    extra_rate_mod=(
                        p["weather_fx"].get("artifact_multiplier", 1.0)
                        * p.get("route_effects", {}).get("artifact_multiplier", 1.0)
                    ),
                )

            # Event from DM (Sonar Pulse skip can suppress it)
            event = None
            sonar_skip_consumed = False
            sonar_skip_active_this_dig = bool(p.get("sonar_skip_active_this_dig"))
            event_preview_skipped = None
            event_id = outcome.get("event_id", "")
            if event_id:
                pool_event = next((e for e in dig_service.EVENT_POOL if e["id"] == event_id), None)
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
                    if sonar_skip_active_this_dig:
                        event_preview_skipped = {
                            "name": event["name"],
                            "description": event["description"],
                            "rarity": event["rarity"],
                        }
                        event = None
                        sonar_skip_consumed = True

            # Void bait decrement
            void_bait_digs = tunnel.get("void_bait_digs", 0) or 0
            if void_bait_digs > 0:
                self.dig_repo.update_tunnel(
                    discord_id, guild_id, void_bait_digs=void_bait_digs - 1,
                )

            # Lantern / Sonar preview + boss scout
            event_preview = None
            boss_scout = None
            if p["has_lantern"] or p["has_sonar_pulse"]:
                preview = self.roll_event(
                    new_depth, luminosity=p["luminosity"], prestige_level=p["prestige_level"],
                    discord_id=discord_id, guild_id=guild_id, in_boss=boss_encounter,
                    tunnel=tunnel,
                )
                if preview:
                    event_preview = {
                        "name": preview.get("name"),
                        "description": preview.get("description"),
                        "rarity": preview.get("rarity", "common"),
                    }
            if p["has_lantern"]:
                for boundary in BOSS_BOUNDARIES:
                    if boundary > new_depth and boundary - new_depth <= 10:
                        boss_scout = {
                            "blocks_until": boundary - new_depth,
                            "depth": boundary,
                        }
                        break
            if sonar_skip_consumed and event_preview is None:
                event_preview = event_preview_skipped
            if sonar_skip_consumed:
                self.dig_repo.update_tunnel(
                    discord_id, guild_id, sonar_skip_pending=0,
                )

            total_digs = (tunnel.get("total_digs", 0) or 0) + 1

            jc_earned = scale_minigame_jc_delta(jc_earned)
            gross_jc = jc_earned
            jc_earned = scale_positive_dig_jc(gross_jc)
            overgrowth_bonus = 10 if p.get("overgrowth_active") else 0
            jc_earned += overgrowth_bonus

            # Plains tithe / Blue tax apply to the scaled payout plus Overgrowth.
            jc_earned = self._apply_mana_yield_taxes(discord_id, guild_id, jc_earned)
            # Helltide bell: flat per-dig tax while the guild modifier is active.
            helltide_tax = scale_deflationary_minigame_jc_delta(self._helltide_tax(guild_id))
            if helltide_tax > 0:
                jc_earned = max(0, jc_earned - helltide_tax)

            # Bankruptcy debuff: keep only the configured fraction of yield while
            # penalized (applied last, before the credit; withheld share is a sink).
            jc_earned, dig_bankruptcy_penalty = self._penalize_jc(discord_id, guild_id, jc_earned)

            # DB writes
            run_jc = (tunnel.get("current_run_jc", 0) or 0) + jc_earned
            run_artifacts = (tunnel.get("current_run_artifacts", 0) or 0) + (1 if artifact else 0)
            run_events_count = (tunnel.get("current_run_events", 0) or 0) + (1 if event else 0)
            # Tunnel advance + JC payout commit together so a crash can't
            # leave the depth written with the player unpaid.
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=jc_earned,
                tunnel_updates={
                    "depth": new_depth, "total_digs": total_digs, "last_dig_at": now,
                    "max_depth": max(prev_max_depth, new_depth),
                    "total_jc_earned": (tunnel.get("total_jc_earned", 0) or 0) + jc_earned,
                    "streak_days": streak, "streak_last_date": today,
                    "cavein_free_streak": cavein_free_streak,
                    "current_run_jc": run_jc,
                    "current_run_artifacts": run_artifacts,
                    "current_run_events": run_events_count,
                },
                consume_inventory_item_ids=p.get("consumed_item_row_ids") or [],
            )
            jc_earned = self._apply_blood_pact_skim_to_payout(discord_id, guild_id, jc_earned)

            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
                depth_before=depth_before,
                depth_after=new_depth,
                jc_delta=jc_earned,
                details=json.dumps({
                    "advance": advance, "jc": jc_earned,
                    "gross_jc": gross_jc,
                    "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
                    "overgrowth_bonus": overgrowth_bonus,
                    "depth_before": depth_before, "depth_after": new_depth,
                    "boss_encounter": boss_encounter, "cave_in": False,
                    "corruption": p["corruption"]["id"] if p["corruption"] else None,
                    "dm_mode": True,
                    "streak_charm_used": streak_charm_used,
                }),
            )
            if p.get("overgrowth_active") and self.buff_service is not None:
                try:
                    self.buff_service.consume_overgrowth_charge(discord_id, guild_id)
                except Exception:
                    logger.exception("Failed to consume Overgrowth charge")

            paid_dig_cost = p["paid_dig_cost"]
            result = self._ok(
                tunnel_name=tunnel.get("tunnel_name") or "Unknown Tunnel",
                depth_before=depth_before, depth_after=new_depth,
                advance=advance, jc_earned=jc_earned, gross_jc=gross_jc,
                overgrowth_bonus=overgrowth_bonus,
                nonstreak_jc=nonstreak_jc,
                bankruptcy_penalty=dig_bankruptcy_penalty,
                milestone_bonus=milestone_bonus, streak_bonus=streak_bonus,
                cave_in=False, cave_in_detail=None,
                boss_encounter=boss_encounter, boss_info=boss_info,
                has_lantern=p["has_lantern"],
                event=event, artifact=artifact,
                is_first_dig=False,
                dig_consumed=True,
                items_used=p["items_used"], items_used_ids=p["items_used_ids"],
                auto_purchases=p.get("auto_purchases", []),
                pickaxe_tier=p["pickaxe_tier"],
                tip=self._pick_tip(new_depth),
                luminosity_info=p["lum_info"],
                paid_cost=paid_dig_cost if paid_dig_cost > 0 else 0,
                corruption=p["corruption"],
                mutations=[m.get("name") for m in p["mutations"]] if p["mutations"] else None,
                event_preview=event_preview,
                boss_scout=boss_scout,
                sonar_skipped=sonar_skip_consumed,
                weather=p["weather_info"],
                streak_charm_used=streak_charm_used,
            )

        return result

    def reset_dig_cooldown(self, discord_id: int, guild_id) -> dict:
        """Admin: reset a player's free dig cooldown."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("That player doesn't have a tunnel.")
        self.dig_repo.update_tunnel(discord_id, guild_id, last_dig_at=0)
        return self._ok(reset=True)
