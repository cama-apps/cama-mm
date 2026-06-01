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
    BOSS_BOUNDARIES,
    CAVE_IN_BLOCK_LOSS_RANGES,
    CAVE_IN_CATASTROPHIC_GEAR_TICKS,
    CAVE_IN_CATASTROPHIC_MEDICAL_BILL,
    CAVE_IN_CATASTROPHIC_MILESTONE_STEP,
    CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE,
    CAVE_IN_INJURY_DIGS_BY_BAND,
    CAVE_IN_MEDICAL_BILL_RANGES,
    CAVE_IN_STUN_DIGS_BY_BAND,
    FREE_DIG_COOLDOWN,
    INJURY_SLOW_COOLDOWN,
    MILESTONES,
    STREAKS,
    cave_in_band,
    pick_cave_in_consequence,
    roll_catastrophic_cave_in,
)


class DigCoreMixin:
    """DigCoreMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """
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
        # Check for stun from injury (overrides base + mutation bonus).
        injury = None
        if tunnel.get("injury_state"):
            try:
                injury = json.loads(tunnel["injury_state"])
            except (json.JSONDecodeError, TypeError):
                injury = None
        if injury and injury.get("type") == "slower_cooldown":
            cooldown = INJURY_SLOW_COOLDOWN
        cooldown = self._apply_stamina_to_cooldown(cooldown, tunnel)
        # Bankruptcy halves whatever cooldown survived. Applied LAST so an
        # injury override (which wipes prior adjustments) still gets halved.
        if self._is_bankrupt(tunnel.get("discord_id"), tunnel.get("guild_id")):
            cooldown //= 2
        remaining = cooldown - elapsed
        return max(0, remaining)

    def _is_bankrupt(self, discord_id, guild_id) -> bool:
        """True if the player currently has bankruptcy penalty games remaining.

        Used to halve the dig cooldown so bankrupt players can grind back
        faster. Falls back to False if the bankruptcy repo isn't wired.
        """
        if self.bankruptcy_repo is None or discord_id is None:
            return False
        try:
            return int(self.bankruptcy_repo.get_penalty_games(int(discord_id), guild_id)) > 0
        except Exception:
            return False

    def _is_unstarted_tunnel(self, tunnel: dict) -> bool:
        """True for profile-created tunnels that have not had a first dig yet."""
        return (
            (tunnel.get("total_digs", 0) or 0) == 0
            and tunnel.get("last_dig_at") is None
            and (tunnel.get("depth", 0) or 0) == 0
        )

    # ── Layer Weather ────────────────────────────────────────────────

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
            is_first_dig=True,
            items_used=[],
            items_used_ids=[],
            pickaxe_tier=0,
            tip="Welcome to the mines! Use /dig again after the cooldown.",
            decay_info=decay_info,
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

    def _apply_cave_in_consequence(
        self,
        *,
        discord_id: int,
        guild_id,
        tunnel: dict,
        depth_before: int,
        band: str,
        block_loss: int,
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
                new_depth = milestone
                tunnel_updates["depth"] = new_depth
            tunnel_updates["temp_buffs"] = None  # nukes any second_wind set above

            cmin, cmax = CAVE_IN_CATASTROPHIC_MEDICAL_BILL
            med_cost = min(random.randint(cmin, cmax), max(0, balance))

            smin, smax = CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE
            stun_digs = random.randint(smin, smax) + injury_bonus
            tunnel_updates["injury_state"] = json.dumps(
                {"type": "slower_cooldown", "digs_remaining": stun_digs}
            )

            for _ in range(CAVE_IN_CATASTROPHIC_GEAR_TICKS):
                try:
                    self.dig_repo.tick_gear_durability(discord_id, guild_id)
                except Exception:
                    logger.debug("catastrophic gear tick failed", exc_info=True)
                    break

            total_block_loss = max(block_loss, depth_before - new_depth)
            insurance_note = " Insurance held the depth." if insurance_saved else ""
            detail = {
                "type": "catastrophic",
                "block_loss": total_block_loss,
                "jc_lost": med_cost,
                "stun_digs": stun_digs,
                "depth_after": new_depth,
                "insurance_saved": insurance_saved,
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
            has_equipped_gear=bool(equipped),
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
            try:
                self.dig_repo.tick_gear_durability(discord_id, guild_id)
            except Exception:
                logger.debug("gear_nick tick failed", exc_info=True)
            return (
                {
                    "type": "gear_nick", "block_loss": block_loss,
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
            if weather_loss_cap is not None:
                block_loss = min(block_loss, int(weather_loss_cap))
            block_loss += int(p["weather_fx"].get("cave_in_loss_bonus", 0))
            block_loss += int(p["mutation_fx"].get("cave_in_loss_bonus", 0))
            steady_hands_reduction = p.get("perk_fx", {}).get("cave_in_loss_reduction", 0.0)
            if steady_hands_reduction > 0:
                block_loss = max(0, int(block_loss * (1.0 - steady_hands_reduction)))
            # Relic: Patient Stone — -30% depth lost
            if self._has_relic(discord_id, guild_id, "patient_stone"):
                block_loss = max(0, int(block_loss * 0.7))
            # Reinforcement: cap cave-in block_loss while the 48h window is active
            reinforced_until_for_cap = tunnel.get("reinforced_until") or 0
            if now < int(reinforced_until_for_cap):
                block_loss = min(block_loss, 8)
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
            )
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
                details=json.dumps({
                    "cave_in": True, "block_loss": block_loss,
                    "detail": cave_in_detail,
                    "depth_before": depth_before, "depth_after": new_depth,
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
        jc_mult = max(0.0, jc_mult - p["ascension"].get("jc_layer_penalty", 0))
        weather_code_now = self._get_weather_code(guild_id, layer_name)
        relic_yield_mult = self._relic_jc_yield_multiplier(
            discord_id, guild_id,
            weather_code=weather_code_now,
            luminosity=luminosity,
            is_first_dig_today=self._is_first_dig_of_day(tunnel.get("last_dig_at"), p["today"]),
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
        if p.get("overgrowth_active"):
            jc_earned += 10

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
        jc_earned += streak_bonus

        # Relic: Prospector's Streak — flat JC per consecutive cave-in-free dig
        # (capped). The counter is bumped here and persisted below; the cave-in
        # branch resets it to 0.
        cavein_free_streak = (tunnel.get("cavein_free_streak", 0) or 0) + 1
        if self._has_relic(discord_id, guild_id, "prospectors_streak"):
            jc_earned += min(cavein_free_streak, 20)

        # Plains tithe / Blue tax apply to the full payout.
        jc_earned = self._apply_mana_yield_taxes(discord_id, guild_id, jc_earned)
        # Helltide bell: flat per-dig tax while the guild modifier is active.
        helltide_tax = self._helltide_tax(guild_id)
        if helltide_tax > 0:
            jc_earned = max(0, jc_earned - helltide_tax)

        # Artifact
        artifact = None
        if not (p["corruption"] and p["corruption"]["effects"].get("skip_artifact")):
            artifact = self.roll_artifact(
                discord_id, guild_id, new_depth,
                extra_rate_mod=p["weather_fx"].get("artifact_multiplier", 1.0),
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
        )
        if self.buff_service is not None and jc_earned > 0:
            try:
                skimmed = self.buff_service.apply_blood_pact_skim(
                    discord_id, guild_id, jc_earned, self.player_repo
                )
                if skimmed:
                    jc_earned = max(0, jc_earned - skimmed)
            except Exception:
                logger.exception("Failed to apply Blood Pact skim to dig payout")
        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id, action_type="dig",
            details=json.dumps({
                "advance": advance, "jc": jc_earned,
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
            advance=advance, jc_earned=jc_earned,
            bankruptcy_penalty=dig_bankruptcy_penalty,
            milestone_bonus=milestone_bonus, streak_bonus=streak_bonus,
            cave_in=False, cave_in_detail=None,
            boss_encounter=boss_encounter, boss_info=boss_info,
            has_lantern=p["has_lantern"],
            event=event, artifact=artifact,
            is_first_dig=False,
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
            if grappling_hook_charges > 0:
                block_loss = 0
                grappling_absorbed = True
                cave_in_tunnel_updates["grappling_hook_charges"] = grappling_hook_charges - 1
            elif p["pickaxe_tier"] >= 7:
                block_loss = max(1, block_loss - 1)
            weather_loss_cap = p["weather_fx"].get("cave_in_loss_cap")
            if weather_loss_cap is not None:
                block_loss = min(block_loss, int(weather_loss_cap))
            # Reinforcement: cap cave-in block_loss while the 48h window is active
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
                cave_in_balance_delta += random.randint(loot_min, loot_max)
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
            jc_earned += streak_bonus

            # Relic: Prospector's Streak — flat JC per consecutive cave-in-free dig
            # (capped). The counter is bumped here and persisted below; the cave-in
            # branch resets it to 0.
            cavein_free_streak = (tunnel.get("cavein_free_streak", 0) or 0) + 1
            if self._has_relic(discord_id, guild_id, "prospectors_streak"):
                jc_earned += min(cavein_free_streak, 20)

            # Artifact (deterministic)
            artifact = None
            if not (p["corruption"] and p["corruption"]["effects"].get("skip_artifact")):
                artifact = self.roll_artifact(
                    discord_id, guild_id, new_depth,
                    extra_rate_mod=p["weather_fx"].get("artifact_multiplier", 1.0),
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

            # Mana × weather combo (Sunny + White) boosts yield. The DM range
            # (jc_min/jc_max) is computed without this combo, so apply it here to
            # match dig() / _execute_deterministic_outcome before taxes.
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

            # Plains tithe / Blue tax apply to the full payout.
            jc_earned = self._apply_mana_yield_taxes(discord_id, guild_id, jc_earned)
            # Helltide bell: flat per-dig tax while the guild modifier is active.
            helltide_tax = self._helltide_tax(guild_id)
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
            )
            if self.buff_service is not None and jc_earned > 0:
                try:
                    skimmed = self.buff_service.apply_blood_pact_skim(
                        discord_id, guild_id, jc_earned, self.player_repo
                    )
                    if skimmed:
                        jc_earned = max(0, jc_earned - skimmed)
                except Exception:
                    logger.exception("Failed to apply Blood Pact skim to dig payout")

            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id, action_type="dig",
                details=json.dumps({
                    "advance": advance, "jc": jc_earned,
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
                advance=advance, jc_earned=jc_earned,
                bankruptcy_penalty=dig_bankruptcy_penalty,
                milestone_bonus=milestone_bonus, streak_bonus=streak_bonus,
                cave_in=False, cave_in_detail=None,
                boss_encounter=boss_encounter, boss_info=boss_info,
                has_lantern=p["has_lantern"],
                event=event, artifact=artifact,
                is_first_dig=False,
                items_used=p["items_used"], items_used_ids=p["items_used_ids"],
                pickaxe_tier=p["pickaxe_tier"],
                tip=self._pick_tip(new_depth),
                decay_info=p["decay_info"],
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
