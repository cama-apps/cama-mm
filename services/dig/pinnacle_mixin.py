"""PinnacleMixin mixin for :class:`DigService`.

All pinnacle-boss logic: the rotating-pool lock, the 3-phase fight
resolver and its resume path, the shared end-of-fight finalizer, the
relic drop / stat-application helpers, and the encounter-info builder.

The pinnacle is a single 3-phase fight at ``PINNACLE_DEPTH``. Each phase
uses a distinct archetype (per ``PINNACLE_BOSSES[id].phases``). Persisted
HP carries between phases. Defeating phase 3 marks the pinnacle
``defeated`` and drops a unique relic with 2 random rolls.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import json
import random
import time

import services.dig_service as dig_service
from services.dig._common import (
    _luminosity_combat_penalty,
)
from services.dig_constants import (
    BOSS_BOUNDARIES,
    BOSS_DIALOGUE_V2,
    BOSS_DUEL_STATS,
    BOSS_FREE_FIGHT_ACCURACY_MOD,
    BOSS_PAYOUTS,
    BOSS_PRESTIGE_BONUS,
    BOSS_ROUND_CAP,
    BOSS_TIER_BONUS,
    LUMINOSITY_MAX,
    PHASE_TRANSITION_EVENTS,
    PINNACLE_BASE_JC_REWARD,
    PINNACLE_BOSSES,
    PINNACLE_DEPTH,
    PINNACLE_FORESHADOW_LINES,
    PINNACLE_JC_PER_PRESTIGE,
    PINNACLE_POOL_IDS,
    PINNACLE_RELIC_BASE_NAME,
    PINNACLE_RELIC_STAT_POOL,
    PINNACLE_RELIC_SUFFIX_POOL,
    PLAYER_HIT_CEILING,
    PLAYER_HIT_FLOOR,
)


class PinnacleMixin:
    """PinnacleMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

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
        boss_progress = self._get_boss_progress_entries(tunnel)

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
        # Snapshot starting HP for the post-loss soften UX.
        starting_boss_hp = int(boss_hp)
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

        # Silent mana variance modifier (parity with fight_boss).
        if self.mana_effects_service is not None:
            try:
                _pin_effects = self.mana_effects_service.get_effects(discord_id, guild_id)
                if (
                    _pin_effects.color is not None
                    and _pin_effects.boss_damage_variance_modifier != 0
                    and random.random() < 0.5
                ):
                    _pin_scale = 1.0 + _pin_effects.boss_damage_variance_modifier
                    player_dmg = max(1, int(player_dmg * _pin_scale))
                    boss_dmg = max(1, int(boss_dmg * _pin_scale))
            except Exception:
                pass

        # Consume the pending phase event (one-shot) so it doesn't fire again.
        if phase_event_obj is not None:
            pin_entry_now.pop("pending_phase_event_id", None)
            boss_progress[str(PINNACLE_DEPTH)] = pin_entry_now

        # Bold/Reckless crit carries through to the pinnacle fight.
        crit_chance = float(stats.get("crit_chance", 0) or 0)
        crit_bonus = int(stats.get("crit_bonus", 0) or 0)

        win_chance = dig_service._approx_duel_win_prob(
            player_hp=player_hp,
            boss_hp=boss_hp,
            player_hit=player_hit,
            player_dmg=player_dmg,
            boss_hit=boss_hit_chance,
            boss_dmg=boss_dmg,
            crit_chance=crit_chance,
            crit_bonus=crit_bonus,
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
                )

            entry: dict = {"round": round_num}
            crit_this_round = False
            if random.random() < player_hit:
                dmg_this_round = player_dmg
                if crit_chance > 0 and random.random() < crit_chance:
                    dmg_this_round += crit_bonus
                    crit_this_round = True
                boss_hp -= dmg_this_round
            entry["crit"] = crit_this_round
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
            starting_boss_hp=starting_boss_hp,
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
        # Crit carries through paused/resumed fights — pull from the
        # risk-tier table since it's not persisted in the duel state.
        _crit_stats = BOSS_DUEL_STATS.get(state_row["risk_tier"], {})
        crit_chance = float(_crit_stats.get("crit_chance", 0) or 0)
        crit_bonus = int(_crit_stats.get("crit_bonus", 0) or 0)

        # Continue remaining auto-rounds if option didn't decide it.
        if won is None:
            for r in range(round_num + 1, BOSS_ROUND_CAP + 1):
                entry: dict = {"round": r}
                crit_this_round = False
                if random.random() < player_hit:
                    dmg_this_round = player_dmg
                    if crit_chance > 0 and random.random() < crit_chance:
                        dmg_this_round += crit_bonus
                        crit_this_round = True
                    boss_hp -= dmg_this_round
                entry["crit"] = crit_this_round
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

        # Soften UX from a resumed pinnacle fight uses the at-pause HP
        # snapshot as the best-effort starting HP for this engagement.
        starting_boss_hp_for_resume = int(state_row.get("boss_hp", 0) or 0)
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
            starting_boss_hp=starting_boss_hp_for_resume,
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
        starting_boss_hp: int | None = None,
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
                # round-by-round offsets (hit/dmg) and boss_hp_delta. The
                # luminosity delta is the only pre-fight effect applied here.
                pin_entry["pending_phase_event_id"] = phase_event.id
                boss_progress[str(PINNACLE_DEPTH)] = pin_entry

                # Lock the wager so phases 2/3 ride the same stake.
                if wager > 0:
                    self._set_carried_wager(
                        boss_progress, PINNACLE_DEPTH, wager, risk_tier,
                    )

                # Apply the pre-fight luminosity effect of the event now
                # (clamped to [0, MAX] on the tunnel). The boss_hp_delta is
                # applied when the next phase's fight consumes
                # pending_phase_event_id (see _fight_pinnacle), mirroring the
                # regular multi-phase boss flow.
                lum_after = self._get_luminosity(tunnel)
                if phase_event.luminosity_delta:
                    lum_after = max(0, min(LUMINOSITY_MAX, lum_after + phase_event.luminosity_delta))

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
                    round_log=round_log,
                    is_pinnacle=True,
                    gear_broken=gear_broken_names,
                    gear_drop=None,
                    luminosity_display=self._luminosity_combat_display(tunnel),
                )

            # Phase 3 win — pinnacle defeated.
            new_depth = PINNACLE_DEPTH
            jc_reward = PINNACLE_BASE_JC_REWARD + PINNACLE_JC_PER_PRESTIGE * prestige_level
            # A carried wager rode all 3 phases; pay it out at win-chance-tapered
            # odds on top of the base reward. Any phase loss already forfeited it.
            wager_payout = 0
            if wager > 0:
                tier_index = {"cautious": 0, "bold": 1, "reckless": 2}.get(risk_tier, 1)
                base_mult = BOSS_PAYOUTS.get(PINNACLE_DEPTH, (2.0, 3.0, 6.0))[tier_index]
                eff_mult = self._effective_wager_multiplier(base_mult, win_chance)
                wager_payout = int(wager * (eff_mult - 1))
            total_reward = jc_reward + wager_payout
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
            self.player_repo.add_balance(discord_id, guild_id, total_reward)
            self.dig_repo.log_action(
                discord_id=discord_id, guild_id=guild_id,
                action_type="pinnacle_fight",
                details=json.dumps({
                    "pinnacle_id": pinnacle_id,
                    "phase": 3, "won": True,
                    "jc_delta": total_reward,
                    "wager_payout": wager_payout,
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
                jc_delta=total_reward,
                payout=total_reward,
                base_reward=jc_reward,
                wager_payout=wager_payout,
                new_depth=new_depth,
                dialogue=f"You stand over the broken form of {pinnacle.name}.",
                pinnacle_relic=relic_drop,
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
        # Forfeited on a loss — drop the carry markers so a retry starts fresh.
        self._clear_carried_wager(boss_progress, PINNACLE_DEPTH)

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
        soften_line = None
        if starting_boss_hp is not None:
            chipped = int(starting_boss_hp) - max(0, int(boss_hp))
            if chipped > 0:
                soften_line = (
                    f"You knocked the boss from {int(starting_boss_hp)}/{boss_hp_max} "
                    f"to {max(0, int(boss_hp))}/{boss_hp_max} before retreating."
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
            soften_line=soften_line,
            dialogue=f"{boss_name} sends you reeling back {knockback} blocks!",
            round_log=round_log,
            is_pinnacle=True,
            gear_broken=gear_broken_names,
            gear_drop=None,
            luminosity_display=self._luminosity_combat_display(tunnel),
        )
