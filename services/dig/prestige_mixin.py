"""PrestigeMixin mixin for :class:`DigService`.

Prestige, run scoring, the hall of fame, and tunnel abandonment.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import json
import random

from repositories.dig_repository import TunnelStateConflictError
from services.dig_constants import (
    ASCENSION_MODIFIERS,
    BOSS_BOUNDARIES,
    DIG_POSITIVE_JC_MULTIPLIER,
    MAX_PRESTIGE,
    MUTATION_BY_ID,
    PINNACLE_DEPTH,
    PRESTIGE_PERK_STACK_CAP,
    PRESTIGE_PERKS,
    scale_positive_dig_jc,
)


class PrestigeMixin:
    """PrestigeMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """
    def _calculate_run_score(self, tunnel: dict) -> int:
        """Calculate prestige run score."""
        depth = tunnel.get("depth", 0) or 0
        boss_progress = self._get_boss_progress(tunnel)
        bosses_defeated = sum(
            1 for v in boss_progress.values()
            if (v.get("status") if isinstance(v, dict) else v) == "defeated"
        )
        run_jc = tunnel.get("current_run_jc", 0) or 0
        run_events = tunnel.get("current_run_events", 0) or 0
        prestige_level = tunnel.get("prestige_level", 0) or 0

        # Artifacts-found no longer contributes to the score — relics are unique
        # rewards, not a per-run collection metric.
        base = (depth * 1 + bosses_defeated * 50
                + int(run_jc * 0.5) + run_events * 10)
        multiplier = 1 + prestige_level * 0.1
        # P10 "The Endless" doubles score multiplier
        ascension = self._get_ascension_effects(prestige_level)
        score_mult = ascension.get("score_multiplier", 0)
        if score_mult:
            multiplier += score_mult
        return int(base * multiplier)

    def get_hall_of_fame(self, guild_id) -> dict:
        """Get guild leaderboard of best prestige run scores."""
        return self.leaderboard_service.get_hall_of_fame(guild_id)

    def preview_abandon(self, discord_id: int, guild_id) -> dict:
        """Preview abandon refund without executing."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        depth = tunnel.get("depth", 0)

        if depth < 10:
            return self._error("Tunnel must be at least 10 blocks deep to abandon.")

        gross_refund = int(depth * 0.1)
        refund = scale_positive_dig_jc(gross_refund)
        return self._ok(
            refund=refund,
            gross_refund=gross_refund,
            current_depth=depth,
        )

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
            available_perks=self._eligible_perks(tunnel),
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
        # Stacks compound additively via _aggregate_perk_effects, capped at 5
        # picks per perk to keep late-prestige scaling sane.
        if current_perks.count(perk_choice) >= PRESTIGE_PERK_STACK_CAP:
            return self._error("You've maxed that perk.")

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

        # Flat prestige grant: 1000 JC + one rare-or-better relic.
        from services.dig_constants import RELICS, TROPHY_RELIC_IDS

        prestige_gross_jc = 1000
        prestige_jc_grant = scale_positive_dig_jc(prestige_gross_jc)
        # Relics are unique and signature trophies are carve-only — exclude
        # owned relics and trophies from the grant pool.
        owned = {
            dict(a).get("artifact_id")
            for a in (self.dig_repo.get_artifacts(discord_id, guild_id) or [])
        }
        eligible_relic_ids = [
            r.id for r in RELICS
            if r.rarity in ("Rare", "Legendary")
            and r.id not in TROPHY_RELIC_IDS
            and r.id not in owned
        ]
        granted_relic = None
        granted_relic_id = None
        if eligible_relic_ids:
            # Roll the relic id WITHOUT inserting it here. The insert is fused
            # into the atomic tunnel-reset/JC-grant below so the relic, the JC
            # grant, and the prestige reset commit (or roll back) together. A
            # standalone add_artifact that committed before a failing reset
            # would mint the relic while leaving prestige_level/boss_progress
            # unreset — can_prestige() stays True and a retry rolls a SECOND
            # relic.
            granted_relic_id = random.choice(eligible_relic_ids)
            granted_def = next(r for r in RELICS if r.id == granted_relic_id)
            granted_relic = {
                "id": granted_relic_id,
                "name": granted_def.name,
                "rarity": granted_def.rarity,
            }
        prestige_grant = {"jc": prestige_jc_grant, "relic": granted_relic}

        # Reset tunnel — including pinnacle state so the next cycle re-rolls
        # a fresh pinnacle from the rotating pool on first encounter. The
        # tunnel reset and the flat JC grant commit together so a crash
        # can't reset the run without paying the grant (or vice versa).
        # The reset is also guarded on the prestige level we validated:
        # two rapid calls can both pass can_prestige before the first reset
        # commits, so the write is conditional — the loser rolls back
        # (no grant, no relic) and gets a clean error below.
        boss_progress = {str(b): "active" for b in BOSS_BOUNDARIES}
        try:
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=prestige_jc_grant,
                add_relic_artifact_id=granted_relic_id,
                require_tunnel_state={"prestige_level": prestige_level - 1},
                tunnel_updates={
                    "depth": 0,
                    "boss_progress": json.dumps(boss_progress),
                    "boss_attempts": 0,
                    "prestige_level": prestige_level,
                    "prestige_perks": json.dumps(current_perks),
                    "cheer_data": None,
                    "injury_state": None,
                    "best_run_score": best_score,
                    "current_run_jc": 0,
                    "current_run_artifacts": 0,
                    "current_run_events": 0,
                    "total_prestige_score": total_score,
                    "mutations": mutations_json,
                    "stat_boss_awards": json.dumps({
                        "prestige_level": prestige_level,
                        "awards": [],
                    }),
                    "pinnacle_boss_id": None,
                    "pinnacle_phase": 0,
                    "pinnacle_hp_remaining": None,
                    "pinnacle_last_engaged_at": None,
                    "route_state": None,
                },
            )
        except TunnelStateConflictError:
            return self._error("Your prestige was already processed.")

        self.dig_repo.log_action(
            discord_id=discord_id, guild_id=guild_id,
            action_type="prestige",
            jc_delta=prestige_jc_grant,
            details=json.dumps({
                "level": prestige_level, "perk": perk_choice,
                "run_score": run_score, "mutations": mutation_info,
                "gross_jc": prestige_gross_jc,
                "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
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
            prestige_grant=prestige_grant,
        )

    # ------------------------------------------------------------------
    # Items
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

        gross_refund = int(depth * 0.1)
        refund = scale_positive_dig_jc(gross_refund)
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
                # Abandon starts a fresh run — clear pinnacle state like
                # prestige does, so the next cycle re-rolls from scratch
                # instead of resuming a stored phase.
                "pinnacle_boss_id": None,
                "pinnacle_phase": 0,
                "pinnacle_hp_remaining": None,
                "pinnacle_last_engaged_at": None,
                "route_state": None,
            },
            log_detail={
                "depth": depth,
                "refund": refund,
                "gross_jc": gross_refund,
                "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
            },
            log_action_type="abandon",
        )

        return self._ok(
            depth_lost=depth,
            refund=refund,
        )

    # ------------------------------------------------------------------
    # Stats & Utility
    # ------------------------------------------------------------------
