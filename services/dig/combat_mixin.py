"""BossCombatMixin mixin for :class:`DigService`.

Boss encounters, multi-phase duels, the pinnacle fight, combat
scaling, and the scout/retreat/cheer flow.

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
    BOSS_ARCHETYPE_BY_ID,
    BOSS_ARCHETYPES,
    BOSS_BOUNDARIES,
    BOSS_DUEL_STATS,
    BOSS_FREE_FIGHT_ACCURACY_MOD,
    BOSS_HP_REGEN_PER_3_HOURS,
    BOSS_LOSS_EXTRA_COOLDOWN_SECONDS,
    BOSS_LOSS_EXTRA_GEAR_TICKS,
    BOSS_LOSS_KNOCKBACK_MAX,
    BOSS_LOSS_KNOCKBACK_MIN,
    BOSS_LOSS_REPAIR_BILL,
    BOSS_NAMES,
    BOSS_PAYOUTS,
    BOSS_PHASE2,
    BOSS_PHASE3,
    BOSS_PHASES,
    BOSS_PRESTIGE_BONUS,
    BOSS_ROUND_CAP,
    BOSS_TIER_BONUS,
    BOSS_VICTORY_BASE_JC,
    CHEER_COOLDOWN_SECONDS,
    PHASE_TRANSITION_EVENTS,
    PINNACLE_BOSSES,
    PINNACLE_DEPTH,
    PINNACLE_REPROC_DEPTH,
    PLAYER_HIT_CEILING,
    PLAYER_HIT_FLOOR,
    RETREAT_BLOCK_LOSS_MAX,
    RETREAT_BLOCK_LOSS_MIN,
    WIN_CHANCE_CAP,
    get_phase2_for,
    get_phase3_for,
)


class BossCombatMixin:
    """BossCombatMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """
    def has_scout_lantern(self, discord_id: int, guild_id) -> bool:
        """True if the player can scout a boss right now.

        Scouting accepts either a Lantern consumable (single-use) or the
        persistent Great Lantern gear. The boss-encounter UI uses this to
        decide whether the Scout button is enabled — ownership semantics,
        not "did the player queue a lantern this dig".
        """
        inv = self.dig_repo.get_inventory(discord_id, guild_id)
        return any(i.get("item_type") in ("lantern", "great_lantern") for i in inv)

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
            of ``BOSS_HP_REGEN_PER_3_HOURS`` per three-hour block since
            ``last_engaged_at``, capped at ``hp_max``;
          - ``fresh_hp`` if no persisted HP exists.

        ``at_boss`` is normally an int boundary depth (e.g. 25), but pinnacle
        callers pass a composite phase key string like ``f"{PINNACLE_DEPTH}:1"``.
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
                three_hour_blocks = max(0, (now - int(last_engaged)) // 10800)
            except (TypeError, ValueError):
                three_hour_blocks = 0
            hp_remaining = min(hp_max, hp_remaining + three_hour_blocks * BOSS_HP_REGEN_PER_3_HOURS)
        return max(1, hp_remaining), hp_max

    def get_carried_wager(self, discord_id: int, guild_id) -> dict | None:
        """Public: return carried-wager state for the player's current boss boundary, or None.

        UI uses this to decide whether to skip the wager modal — a phase 2/3
        encounter inherits the original wager + risk_tier from phase 1.
        Returns ``{wager, risk_tier, boundary}`` when present.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return None
        boss_progress = self._get_boss_progress_entries(dict(tunnel))
        depth = tunnel["depth"] if "depth" in tunnel.keys() else 0
        at_boss = self._at_boss_boundary(depth, boss_progress)
        if at_boss is None:
            return None
        carried = self._get_carried_wager(boss_progress, at_boss)
        if carried is None:
            return None
        if not self._is_pinnacle_depth(at_boss):
            self._clear_carried_wager(boss_progress, at_boss)
            self.dig_repo.update_tunnel(
                discord_id, guild_id, boss_progress=json.dumps(boss_progress),
            )
            return None
        wager, risk_tier = carried
        return {"wager": wager, "risk_tier": risk_tier, "boundary": at_boss}

    def _pick_boss_outcome_line(
        self, *, boss=None, boss_name: str = "the boss",
        boundary: int | None = None, won: bool,
    ) -> str:
        """Random pick from per-boss victory/defeat pools (or generic fallback).

        ``boss`` is a BossDef when available (from _ensure_boss_locked); we
        fall back to looking up the boundary in BOSSES if not. ``{boss}``
        tokens in the line are substituted with ``boss_name``.
        """
        from services.dig_constants import (
            BOSSES,
            GENERIC_DEFEAT_LINES,
            GENERIC_VICTORY_LINES,
        )
        bd = boss if boss is not None else (
            BOSSES.get(boundary) if boundary is not None else None
        )
        pool = ()
        if bd is not None:
            pool = bd.victory_lines if won else bd.defeat_lines
        if not pool:
            pool = GENERIC_VICTORY_LINES if won else GENERIC_DEFEAT_LINES
        line = random.choice(pool)
        return line.replace("{boss}", boss_name)

    # Flat denominator for the wager-skin bonus. Going all-in with a small
    # balance does NOT max the bonus — only stakes that meet this threshold
    # do, by design.
    _WAGER_SKIN_BONUS_DENOMINATOR: int = 500
    _WAGER_SKIN_BONUS_MAX: float = 0.03

    # Wager payouts taper toward break-even once a fight is a near-sure thing:
    # at or below this win chance the authored BOSS_PAYOUTS multiplier is
    # untouched; above it the multiplier blends down to fair odds, so softening
    # a boss to ~95% then betting big no longer prints money.
    _WAGER_TAPER_KNEE: float = 0.65

    def _wager_skin_bonus(self, wager: int) -> float:
        """Silent +0..+3% hit bonus that scales with wager size.

        ``wager_ratio = min(1.0, wager / 500)`` so a 500+ JC wager maxes the
        bonus. No UI surface — this only nudges the per-round hit roll. Free
        fights (wager == 0) get nothing.
        """
        if wager <= 0:
            return 0.0
        ratio = min(1.0, wager / float(self._WAGER_SKIN_BONUS_DENOMINATOR))
        return ratio * self._WAGER_SKIN_BONUS_MAX

    def _effective_wager_multiplier(
        self, base_multiplier: float, win_chance: float,
    ) -> float:
        """Taper the wager payout multiplier toward break-even at high win chance.

        At or below ``_WAGER_TAPER_KNEE`` the authored ``base_multiplier`` is
        returned unchanged, so normal and genuinely-risky betting is unaffected.
        Above the knee it blends linearly toward fair odds (``1 / win_chance``)
        at ``WIN_CHANCE_CAP``, where a wager is EV-neutral. The taper only ever
        reduces a payout, never raises one.
        """
        knee = self._WAGER_TAPER_KNEE
        if win_chance <= knee:
            return base_multiplier
        fair = 1.0 / win_chance
        span = max(1e-6, WIN_CHANCE_CAP - knee)
        t = min(1.0, (win_chance - knee) / span)
        eff = base_multiplier * (1.0 - t) + fair * t
        return min(base_multiplier, eff)

    def _get_carried_wager(self, boss_progress: dict, at_boss) -> tuple[int, str] | None:
        """Return (wager, risk_tier) carried from a prior phase win, or None.

        A multi-phase boss fight stores the original wager + risk_tier on the
        boss_progress entry when phase 1 (or 2) is cleared, so the next phase
        rides the same stake. Returns None when there is no carry.
        """
        entry = boss_progress.get(str(at_boss))
        if not isinstance(entry, dict):
            return None
        cw = entry.get("carried_wager")
        crt = entry.get("carried_risk_tier")
        if cw is None or crt is None:
            return None
        try:
            return (int(cw), str(crt))
        except (TypeError, ValueError):
            return None

    def _set_carried_wager(
        self, boss_progress: dict, at_boss, wager: int, risk_tier: str,
    ) -> None:
        """Store a wager + risk_tier on the boss_progress entry for next phase."""
        entry = boss_progress.get(str(at_boss))
        if isinstance(entry, dict):
            entry = dict(entry)
        elif isinstance(entry, str):
            entry = {"status": entry}
        else:
            entry = {}
        entry["carried_wager"] = int(wager)
        entry["carried_risk_tier"] = str(risk_tier)
        boss_progress[str(at_boss)] = entry

    def _clear_carried_wager(self, boss_progress: dict, at_boss) -> None:
        """Drop carried_wager / carried_risk_tier from a boss_progress entry."""
        entry = boss_progress.get(str(at_boss))
        if isinstance(entry, dict):
            entry.pop("carried_wager", None)
            entry.pop("carried_risk_tier", None)
            boss_progress[str(at_boss)] = entry

    def _regular_boss_wager_allowed(
        self, boss_progress: dict, at_boss: int, prestige_level: int,
    ) -> bool:
        """Return True only when this regular boss fight is on its final phase."""
        entry = boss_progress.get(str(at_boss), "active")
        current_status = entry.get("status", "active") if isinstance(entry, dict) else entry
        phase2_min_p = int(BOSS_PHASES.get("phase_2_min_prestige", 2))
        phase3_min_p = int(BOSS_PHASES.get("phase_3_min_prestige", 5))
        phase3_min_tier = int(BOSS_PHASES.get("phase_3_min_tier", 100))
        has_later_phase = (
            prestige_level >= phase2_min_p
            and at_boss in BOSS_PHASE2
            and current_status == "active"
        ) or (
            prestige_level >= phase3_min_p
            and at_boss >= phase3_min_tier
            and at_boss in BOSS_PHASE3
            and current_status == "phase1_defeated"
        )
        return not has_later_phase

    def _forced_no_wager_regular_phase(
        self, boss_progress: dict, at_boss: int, prestige_level: int, wager: int,
    ) -> bool:
        """True when a regular boss phase forbids wagering, making wager=0 mandatory."""
        return (
            wager == 0
            and not self._is_pinnacle_depth(at_boss)
            and not self._regular_boss_wager_allowed(
                boss_progress, at_boss, prestige_level,
            )
        )

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
        string like ``f"{PINNACLE_DEPTH}:1"``.
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

        # Boundary key for the tier lookup. Pinnacle uses PINNACLE_DEPTH; for
        # off-boundary calls (defensive), pick the highest boundary <= at_boss.
        tier_key = at_boss if at_boss in BOSS_TIER_BONUS else max(
            (k for k in BOSS_TIER_BONUS if k <= at_boss), default=25,
        )
        tier = BOSS_TIER_BONUS[tier_key]
        prestige = BOSS_PRESTIGE_BONUS.get(prestige_level, BOSS_PRESTIGE_BONUS[max(BOSS_PRESTIGE_BONUS)])

        # Boss HP: archetype mult, then tier+prestige adds from tables, then echo.
        boss_hp = float(stats["boss_hp"]) * archetype["hp_mult"]
        boss_hp += tier["hp"] + prestige["hp"]
        # P4+ delvers face slightly tougher bosses — a small extra HP curve on
        # top of the flat prestige table ("Boss Rage" made real).
        if prestige_level >= 4:
            boss_hp *= 1.0 + 0.03 * (prestige_level - 3)
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
        prestige_level = int(tunnel.get("prestige_level", 0) or 0)
        pool = _pool(depth, prestige_level=prestige_level)
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

        prestige_level = int(tunnel.get("prestige_level", 0) or 0)
        pool = _pool(depth, prestige_level=prestige_level)
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

    def _next_boss_boundary(self, boss_progress: dict) -> int | None:
        """Return the lowest undefeated boss boundary, regardless of current depth.

        A boundary is "undefeated" if its status is still active or only
        partially cleared (phase1_defeated / phase2_defeated). Returning
        boundaries the player has already crossed lets the dig flow cap
        advance and re-fire the missed encounter.
        """
        for b in sorted(BOSS_BOUNDARIES):
            entry = boss_progress.get(str(b))
            status = entry.get("status") if isinstance(entry, dict) else entry
            if status in ("active", "phase1_defeated", "phase2_defeated"):
                return b
        return None

    def _at_boss_boundary(self, depth: int, boss_progress: dict) -> int | None:
        """Return the boss boundary if the player owes a fight at it.

        Fires when depth has reached or passed an unfinished boundary
        (active / phase1_defeated / phase2_defeated). The ``>= b - 1``
        check covers both the normal arrival case (depth == b - 1) and
        the parked-past case (depth >= b) where a previous skip left
        the boss undefeated.

        The pinnacle (depth 300) is gated: it only fires once all 7
        prior tier bosses are marked defeated.
        """
        for b in BOSS_BOUNDARIES:
            entry = boss_progress.get(str(b))
            status = entry.get("status") if isinstance(entry, dict) else entry
            if depth >= b - 1 and status in ("active", "phase1_defeated", "phase2_defeated"):
                return b
        # Pinnacle: triggers when depth has reached or passed
        # PINNACLE_DEPTH-1 with the pinnacle still unfinished. The
        # original PINNACLE_REPROC_DEPTH window is preserved as a
        # belt-and-braces catch-up for very-deep legacy tunnels.
        at_pinnacle_threshold = depth >= PINNACLE_DEPTH - 1
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
        boss_progress = self._get_boss_progress_entries(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)

        if at_boss is None:
            return self._error("You're not at a boss boundary.")

        # Multi-phase carry: prior phase win locks original wager + risk on
        # the boss_progress entry — the next phase rides the same stake.
        carried = self._get_carried_wager(boss_progress, at_boss)
        if carried is not None and self._is_pinnacle_depth(at_boss):
            wager, risk_tier = carried
        elif carried is not None:
            self._clear_carried_wager(boss_progress, at_boss)
            self.dig_repo.update_tunnel(
                discord_id, guild_id, boss_progress=json.dumps(boss_progress),
            )
            carried = None

        if risk_tier not in ("cautious", "bold", "reckless"):
            return self._error("Invalid risk tier. Choose: cautious, bold, reckless.")

        if wager < 0:
            return self._error("Wager must be non-negative.")

        if wager > 0 and carried is None:
            prestige_level = tunnel.get("prestige_level", 0) or 0
            if (
                not self._is_pinnacle_depth(at_boss)
                and not self._regular_boss_wager_allowed(
                    boss_progress, at_boss, prestige_level,
                )
            ):
                return self._error("Boss wagers are only available on the final phase.")
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
        forced_no_wager_phase = self._forced_no_wager_regular_phase(
            boss_progress, at_boss, prestige_level, wager,
        )

        # Cheer bonus (existing mechanic: +5% accuracy per cheer, cap 3 cheers).
        cheers = self._get_cheers(tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        # Phase accuracy penalty: phase 2 (status phase1_defeated) reads
        # BOSS_PHASE2; phase 3 (status phase2_defeated) reads BOSS_PHASE3.
        phase2_penalty = 0.0
        _phase_entry = boss_progress.get(str(at_boss))
        _phase_status = (
            _phase_entry.get("status") if isinstance(_phase_entry, dict)
            else _phase_entry
        )
        _phase_boss_id = (
            _phase_entry.get("boss_id", "") if isinstance(_phase_entry, dict) else ""
        )
        if _phase_status == "phase1_defeated" and at_boss in BOSS_PHASE2:
            _ph = get_phase2_for(_phase_boss_id, at_boss)
            phase2_penalty = abs(_ph.win_odds_penalty) if _ph else 0.0
        elif _phase_status == "phase2_defeated" and at_boss in BOSS_PHASE3:
            _ph = get_phase3_for(_phase_boss_id, at_boss)
            phase2_penalty = abs(_ph.win_odds_penalty) if _ph else 0.0

        # Pending phase-transition event (rolled when this boss entered its
        # next phase): a one-shot environmental effect applied to this fight.
        phase_event_obj = None
        if isinstance(_phase_entry, dict) and _phase_entry.get("pending_phase_event_id"):
            phase_event_obj = next(
                (e for e in PHASE_TRANSITION_EVENTS
                 if e.id == _phase_entry["pending_phase_event_id"]), None,
            )

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
        if phase_event_obj is not None:
            fresh_boss_hp = max(1, fresh_boss_hp + int(phase_event_obj.boss_hp_delta))
        # Mana: Black inflates fresh boss HP (+30%) for the matching loot
        # bonus applied at payout. White's damage bump is applied to player
        # _dmg below — not here — to keep the symmetry visible in the duel.
        if self.mana_effects_service is not None:
            try:
                _hp_effects = self.mana_effects_service.get_effects(discord_id, guild_id)
                if _hp_effects.color is not None and _hp_effects.boss_hp_mult != 1.0:
                    fresh_boss_hp = max(1, int(fresh_boss_hp * _hp_effects.boss_hp_mult))
            except Exception:
                pass
        # Carry over persisted HP from prior unfinished engagements with regen.
        boss_hp, boss_hp_max = self._resolve_persisted_boss_hp(
            boss_progress, at_boss, fresh_boss_hp, now,
        )
        # Snapshot for the post-loss soften UX ("knocked from X to Y").
        starting_boss_hp = int(boss_hp)
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
            + self._wager_skin_bonus(wager)
        )
        if wager == 0 and not forced_no_wager_phase:
            player_hit *= BOSS_FREE_FIGHT_ACCURACY_MOD
        player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))

        player_hp = int(stats["player_hp"])
        player_dmg = int(stats["player_dmg"])

        # Relic: Hollow Fang — +15% damage vs bosses
        if self._has_relic(discord_id, guild_id, "hollow_fang"):
            player_dmg = max(1, int(player_dmg * 1.15))

        # Silent mana variance modifier on damage values. Mountain bumps both
        # sides on a coin flip (more swing); Forest narrows on a coin flip
        # (steadier). Same EV in expectation; players see this as RNG.
        # White +20% holy strike applies always; Black +30% HP applies to
        # the boss's *fresh* HP (handled near boss_hp resolution); Black
        # +25% loot is applied at payout time. Green nullifies boss crit
        # bonus modifier.
        if self.mana_effects_service is not None:
            try:
                _bf_effects = self.mana_effects_service.get_effects(discord_id, guild_id)
                if (
                    _bf_effects.color is not None
                    and _bf_effects.boss_damage_variance_modifier != 0
                    and random.random() < 0.5
                ):
                    _scale = 1.0 + _bf_effects.boss_damage_variance_modifier
                    player_dmg = max(1, int(player_dmg * _scale))
                    boss_dmg = max(1, int(boss_dmg * _scale))
                # Persistent damage multiplier (White holy strike)
                if _bf_effects.color is not None and _bf_effects.boss_damage_mult != 1.0:
                    player_dmg = max(1, int(player_dmg * _bf_effects.boss_damage_mult))
                # Green: bosses can't crit you — neutralise lum_dmg_bonus
                if _bf_effects.color is not None and _bf_effects.boss_no_crit_against:
                    boss_dmg = max(1, boss_dmg - lum_dmg_bonus)
            except Exception:
                pass

        # Bold/Reckless crit: roll a chance to add bonus damage on a player hit.
        crit_chance = float(stats.get("crit_chance", 0) or 0)
        crit_bonus = int(stats.get("crit_bonus", 0) or 0)

        # Apply the pending phase-transition event's combat effects (one-shot).
        # boss_hp_delta was already folded into fresh_boss_hp above; the rest
        # adjust the stats feeding win_chance and the round loop. Consumed here
        # so a retry of this phase doesn't re-trigger it.
        if phase_event_obj is not None:
            boss_hit_chance = max(
                0.05, min(0.95, boss_hit_chance + float(phase_event_obj.boss_hit_offset)),
            )
            boss_dmg = max(1, boss_dmg + int(phase_event_obj.boss_dmg_delta))
            player_hit += float(phase_event_obj.player_hit_offset)
            player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))
            player_hp = max(1, player_hp + int(phase_event_obj.player_hp_delta))
            player_dmg = max(0, player_dmg + int(phase_event_obj.player_dmg_delta))
            _phase_entry.pop("pending_phase_event_id", None)
            boss_progress[str(at_boss)] = _phase_entry

        player_hp, player_hit, crit_chance, _shifting_idol_bonus = (
            self._apply_shifting_idol_stats(
                discord_id,
                guild_id,
                player_hp,
                player_hit,
                crit_chance,
            )
        )
        bottled_quake_ready = self._has_relic(
            discord_id, guild_id, "bottled_quake",
        )
        legacy_status = self._trophy_status_seed(
            discord_id, guild_id, player_start_hp=player_hp,
        )

        # Estimate actual win probability via Monte Carlo on the entry
        # stats so the returned ``win_chance`` matches what ``scout_boss``
        # would show — per-round hit rate is not the same as duel win rate.
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
        # Wager payout tapers toward break-even once the fight is near-certain,
        # so softening a boss then betting big no longer prints money.
        multiplier = self._effective_wager_multiplier(multiplier, win_chance)

        round_log: list[dict] = []
        won: bool | None = None
        for round_num in range(1, BOSS_ROUND_CAP + 1):
            entry: dict = {"round": round_num}
            player_roll = random.random() < player_hit
            crit_this_round = False
            if player_roll:
                dmg_this_round = player_dmg
                if bottled_quake_ready:
                    dmg_this_round += 1
                    bottled_quake_ready = False
                    entry["bottled_quake"] = True
                if crit_chance > 0 and random.random() < crit_chance:
                    dmg_this_round += crit_bonus
                    crit_this_round = True
                    if legacy_status.get("gear_heal_first_crit"):
                        player_hp = min(
                            int(legacy_status.get("trophy_start_hp", player_hp + 1)),
                            player_hp + 1,
                        )
                        legacy_status["gear_heal_first_crit"] = False
                        entry["blood_locket_heal"] = True
                boss_hp -= dmg_this_round
            entry["player_hit"] = player_roll
            entry["crit"] = crit_this_round
            entry["boss_hp"] = max(0, boss_hp)
            if boss_hp <= 0:
                won = True
                round_log.append(entry)
                break
            boss_roll = random.random() < boss_hit_chance
            if boss_roll:
                player_hp -= boss_dmg
                if legacy_status.get("gear_reflect_first_hit"):
                    boss_hp -= 1
                    legacy_status["gear_reflect_first_hit"] = False
                    entry["briarplate_reflect"] = True
            elif legacy_status.get("gear_springheel_counter"):
                boss_hp -= 1
                legacy_status["gear_springheel_counter"] = False
                entry["springheel_counter"] = True
            entry["boss_hit"] = boss_roll
            entry["player_hp"] = max(0, player_hp)
            entry["boss_hp"] = max(0, boss_hp)
            round_log.append(entry)
            if boss_hp <= 0:
                won = True
                break
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
            for piece in (loadout.weapon, loadout.armor, loadout.boots, loadout.amulet):
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
            _cur_entry = boss_progress.get(str(at_boss), "active")
            current_status = (
                _cur_entry.get("status", "active") if isinstance(_cur_entry, dict)
                else _cur_entry
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
                # Phase transition — boss transforms, fight again. Tunnel
                # update + audit log commit together via atomic helper.
                next_status = "phase1_defeated" if needs_phase2 else "phase2_defeated"
                phase_def = (
                    get_phase2_for(active_boss_id, at_boss) if needs_phase2
                    else get_phase3_for(active_boss_id, at_boss)
                )
                next_phase_num = 2 if needs_phase2 else 3
                # Roll an environmental transition event. Its flavor surfaces
                # in the embed; the event id rides on the boss entry so its
                # mechanical effects are applied at the start of the next
                # phase's fight (and consumed there).
                phase_event = random.choice(PHASE_TRANSITION_EVENTS)
                _prev_entry = boss_progress.get(str(at_boss))
                if isinstance(_prev_entry, dict):
                    _next_entry = dict(_prev_entry)
                    _next_entry["status"] = next_status
                    _next_entry.pop("hp_remaining", None)
                    _next_entry.pop("hp_max", None)
                    _next_entry["pending_phase_event_id"] = phase_event.id
                    boss_progress[str(at_boss)] = _next_entry
                else:
                    boss_progress[str(at_boss)] = {
                        "status": next_status,
                        "pending_phase_event_id": phase_event.id,
                    }
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
                    round_log=round_log,
                    echo_applied=echo_applied,
                    echo_killer_id=active_echo.get("killer_discord_id") if echo_applied else None,
                    gear_broken=gear_broken_names,
                    gear_drop=None,
                )

            # Full victory (or phase 2 already cleared)
            new_depth = at_boss
            echo_payout_mult = 0.7 if echo_applied else 1.0

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
            # Carried wager is settled by the payout below; drop the markers.
            self._clear_carried_wager(boss_progress, at_boss)
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
            stat_award_updates = self._boss_stat_point_award_updates(tunnel, at_boss)
            stat_point_awarded = stat_award_updates is not None
            if stat_award_updates is not None:
                tunnel_updates.update(stat_award_updates)

            # Every boss victory pays a flat depth-scaled base reward so a
            # win is never empty; a wagered win adds its taper-floored profit
            # on top.
            base_reward = BOSS_VICTORY_BASE_JC.get(at_boss, 15)
            if wager > 0:
                # A won wager never returns less than the stake — the taper
                # plus loot penalties (echo) can otherwise drive it negative.
                wager_profit = max(
                    0,
                    int(wager * (multiplier * boss_payout_mult * echo_payout_mult - 1)),
                )
            else:
                wager_profit = 0
            payout_delta = base_reward + wager_profit

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
                    "wager": wager, "jc_delta": payout_delta,
                    "stat_point_awarded": stat_point_awarded,
                    "echo_applied": echo_applied,
                    "rounds": round_log,
                },
            )

            defeat_msg = self._pick_boss_outcome_line(
                boundary=at_boss, boss_name=boss_name, won=True,
            )

            # Roll a possible gear drop on the full kill. Phase-1 transitions
            # do NOT roll — only completed kills.
            gear_drop = self._maybe_drop_gear(discord_id, guild_id, at_boss)
            prestige_relic_drop = self._maybe_drop_prestige_relic(
                discord_id, guild_id, tunnel.get("prestige_level", 0) or 0,
            )


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
                jc_delta=payout_delta,
                payout=payout_delta,
                new_depth=new_depth,
                dialogue=defeat_msg,
                stat_point_awarded=stat_point_awarded,
                round_log=round_log,
                echo_applied=echo_applied,
                echo_killer_id=active_echo.get("killer_discord_id") if echo_applied else None,
                gear_broken=gear_broken_names,
                gear_drop=gear_drop,
                prestige_relic_drop=prestige_relic_drop,
                luminosity_display=self._luminosity_combat_display(tunnel),
            )
        else:
            knockback = random.randint(BOSS_LOSS_KNOCKBACK_MIN, BOSS_LOSS_KNOCKBACK_MAX)
            new_depth = max(0, depth - knockback)
            # A loss always costs something when the player elected a wager/free
            # fight. Forced no-wager phase fights should not pay the free-fight
            # repair bill just because wagering is disallowed.
            jc_delta = (
                -wager if wager > 0
                else 0 if forced_no_wager_phase
                else -BOSS_LOSS_REPAIR_BILL
            )

            # Loss is harsher on gear — an extra durability tick beyond the
            # per-fight tick above.
            name_by_id = {
                p.id: p.tier_def.name
                for p in (loadout.weapon, loadout.armor, loadout.boots, loadout.amulet)
                if p is not None
            }
            for _ in range(BOSS_LOSS_EXTRA_GEAR_TICKS):
                for i in self.dig_repo.tick_gear_durability(discord_id, guild_id):
                    gear_broken_names.append(name_by_id.get(i, "a piece of gear"))

            # Persist post-fight boss HP so soften-and-retreat strategies work.
            # Mutates boss_progress in place to a dict with hp_remaining/last_engaged_at.
            self._persist_boss_hp_after_fight(
                boss_progress, at_boss, active_boss_id,
                ending_hp=max(0, boss_hp), hp_max=boss_hp_max,
                won=False, outcome="loss", now=now,
            )
            # Loss forfeits the carried wager; drop the markers so the next
            # encounter starts fresh.
            self._clear_carried_wager(boss_progress, at_boss)

            # Tunnel knockback + wager forfeit + audit log commit together.
            # The old flow could forfeit the wager without recording the
            # knockback (or vice versa) on a crash.
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id, guild_id,
                balance_delta=jc_delta,
                tunnel_updates={
                    "depth": new_depth,
                    "boss_progress": json.dumps(boss_progress),
                    "boss_attempts": attempts,
                    "cheer_data": None,     # clear cheers on defeat
                    "last_dig_at": now + BOSS_LOSS_EXTRA_COOLDOWN_SECONDS,
                },
                log_detail={
                    "boundary": at_boss, "won": False, "risk": risk_tier,
                    "wager": wager, "knockback": knockback,
                    "rounds": round_log, "boss_hp_remaining": max(0, boss_hp),
                },
                log_action_type="boss_fight",
            )

            soften_line = None
            chipped = starting_boss_hp - max(0, int(boss_hp))
            if chipped > 0:
                soften_line = (
                    f"You knocked the boss from {starting_boss_hp}/{boss_hp_max} "
                    f"to {max(0, int(boss_hp))}/{boss_hp_max} before retreating."
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
                soften_line=soften_line,
                dialogue=self._pick_boss_outcome_line(
                    boundary=at_boss, boss_name=boss_name, won=False,
                ),
                round_log=round_log,
                echo_applied=echo_applied,
                echo_killer_id=active_echo.get("killer_discord_id") if echo_applied else None,
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
        # and clear the row before starting a fresh duel. Tick the gear that
        # actually fought the stale duel (the start-time snapshot persisted in
        # status_effects), not the player's CURRENT loadout — they may have
        # swapped/repaired gear during the abandoned pause, so ticking the
        # live equipment would burn durability on pieces that never fought.
        stale = self.dig_repo.get_active_duel(discord_id, guild_id)
        if stale is not None:
            stale_snapshot_ids: list[int] = []
            try:
                _se = json.loads(stale.get("status_effects") or "{}")
                stale_snapshot_ids = [int(g) for g in (_se.get("gear_snapshot_ids") or [])]
            except (json.JSONDecodeError, TypeError, ValueError):
                stale_snapshot_ids = []
            if stale_snapshot_ids:
                self.dig_repo.tick_gear_durability_ids(stale_snapshot_ids)
            else:
                # Legacy rows without a snapshot fall back to the live loadout.
                self.dig_repo.tick_gear_durability(discord_id, guild_id)
            self.dig_repo.clear_active_duel(discord_id, guild_id)

        boss_progress = self._get_boss_progress_entries(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)
        if at_boss is None:
            return self._error("You're not at a boss boundary.")
        # Multi-phase carry: a prior phase win locks the original wager + risk
        # onto the boss_progress entry. The next phase fight rides the same
        # stake — caller args are ignored when a carry is present.
        carried = self._get_carried_wager(boss_progress, at_boss)
        if carried is not None and self._is_pinnacle_depth(at_boss):
            wager, risk_tier = carried
        elif carried is not None:
            self._clear_carried_wager(boss_progress, at_boss)
            self.dig_repo.update_tunnel(
                discord_id, guild_id, boss_progress=json.dumps(boss_progress),
            )
            carried = None
        if risk_tier not in ("cautious", "bold", "reckless"):
            return self._error("Invalid risk tier. Choose: cautious, bold, reckless.")
        if wager < 0:
            return self._error("Wager must be non-negative.")
        if wager > 0 and carried is None:
            prestige_level = tunnel.get("prestige_level", 0) or 0
            if (
                not self._is_pinnacle_depth(at_boss)
                and not self._regular_boss_wager_allowed(
                    boss_progress, at_boss, prestige_level,
                )
            ):
                return self._error("Boss wagers are only available on the final phase.")
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
        forced_no_wager_phase = self._forced_no_wager_regular_phase(
            boss_progress, at_boss, prestige_level, wager,
        )
        cheers = self._get_cheers(tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        cheer_bonus = min(0.15, len(active_cheers) * 0.05)

        phase2_penalty = 0.0
        _phase_entry = boss_progress.get(str(at_boss))
        _phase_status = (
            _phase_entry.get("status") if isinstance(_phase_entry, dict)
            else _phase_entry
        )
        _phase_boss_id = (
            _phase_entry.get("boss_id", "") if isinstance(_phase_entry, dict) else ""
        )
        if _phase_status == "phase1_defeated" and at_boss in BOSS_PHASE2:
            _ph = get_phase2_for(_phase_boss_id, at_boss)
            phase2_penalty = abs(_ph.win_odds_penalty) if _ph else 0.0
        elif _phase_status == "phase2_defeated" and at_boss in BOSS_PHASE3:
            _ph = get_phase3_for(_phase_boss_id, at_boss)
            phase2_penalty = abs(_ph.win_odds_penalty) if _ph else 0.0

        # Pending phase-transition event (rolled when this boss entered its
        # next phase): a one-shot environmental effect applied to this fight.
        phase_event_obj = None
        if isinstance(_phase_entry, dict) and _phase_entry.get("pending_phase_event_id"):
            phase_event_obj = next(
                (e for e in PHASE_TRANSITION_EVENTS
                 if e.id == _phase_entry["pending_phase_event_id"]), None,
            )

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
        if phase_event_obj is not None:
            fresh_boss_hp = max(1, fresh_boss_hp + int(phase_event_obj.boss_hp_delta))
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
            + self._wager_skin_bonus(wager)
        )
        if wager == 0 and not forced_no_wager_phase:
            player_hit *= BOSS_FREE_FIGHT_ACCURACY_MOD
        player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))

        player_hp = int(stats["player_hp"])
        player_dmg = int(stats["player_dmg"])
        crit_chance = float(stats.get("crit_chance", 0) or 0)
        crit_bonus = int(stats.get("crit_bonus", 0) or 0)

        # Apply the pending phase-transition event's combat effects (one-shot).
        # boss_hp_delta was already folded into fresh_boss_hp above; the rest
        # adjust the stats feeding win_chance and the round loop. Consumed here
        # so a retry of this phase doesn't re-trigger it.
        if phase_event_obj is not None:
            boss_hit_chance = max(
                0.05, min(0.95, boss_hit_chance + float(phase_event_obj.boss_hit_offset)),
            )
            boss_dmg = max(1, boss_dmg + int(phase_event_obj.boss_dmg_delta))
            player_hit += float(phase_event_obj.player_hit_offset)
            player_hit = max(PLAYER_HIT_FLOOR, min(PLAYER_HIT_CEILING, player_hit))
            player_hp = max(1, player_hp + int(phase_event_obj.player_hp_delta))
            player_dmg = max(0, player_dmg + int(phase_event_obj.player_dmg_delta))
            _phase_entry.pop("pending_phase_event_id", None)
            boss_progress[str(at_boss)] = _phase_entry
            # start_boss_duel can pause mid-fight before its post-fight
            # boss_progress write; persist the consumption now so a resumed
            # fight (which re-reads boss_progress) can't re-fire the one-shot.
            self.dig_repo.update_tunnel(
                discord_id, guild_id, boss_progress=json.dumps(boss_progress),
            )

        player_hp, player_hit, crit_chance, shifting_idol_bonus = (
            self._apply_shifting_idol_stats(
                discord_id,
                guild_id,
                player_hp,
                player_hit,
                crit_chance,
            )
        )
        player_hp_max = player_hp

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
        # Wager payout tapers toward break-even once the fight is near-certain.
        multiplier = self._effective_wager_multiplier(multiplier, win_chance)
        attempts = (tunnel.get("boss_attempts", 0) or 0) + 1
        # Snapshot pre-fight boss HP for the post-loss "you knocked it from X
        # to Y" soften UX. Auto-resolve path skips paused state, so the
        # snapshot lives in a local.
        starting_boss_hp = int(boss_hp)

        # Run auto-rounds until trigger or resolution.
        round_log: list[dict] = []
        status_effects: dict = self._trophy_status_seed(
            discord_id, guild_id, player_start_hp=player_hp,
        )
        if shifting_idol_bonus:
            status_effects["shifting_idol_bonus"] = shifting_idol_bonus
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
                            for p in (loadout.weapon, loadout.armor, loadout.boots, loadout.amulet)
                            if p is not None
                        ],
                        "forced_no_wager_phase": forced_no_wager_phase,
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
                    "crit_chance": crit_chance,
                    "crit_bonus": crit_bonus,
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
                    player_hp_max=player_hp_max,
                    boss_hp=boss_hp,
                    boss_hp_max=int(boss_hp_max),
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
                crit_chance=crit_chance, crit_bonus=crit_bonus,
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
            starting_boss_hp=starting_boss_hp,
            forced_no_wager_phase=forced_no_wager_phase,
        )

    def resume_boss_duel(
        self, discord_id: int, guild_id, option_idx: int,
        *, state_row: dict | None = None,
    ) -> dict:
        """Resume a paused duel after the player picks a reactive option.

        ``state_row`` may be supplied by the caller when the row was already
        atomically claimed. When omitted, the row is atomically claimed here
        (read-and-delete) so two concurrent resumes can't both resolve the same
        duel and double-pay / double-drop — the first caller wins the row, the
        second gets ``None`` and bails.
        """
        from domain.models.boss_mechanics import get_mechanic as _get_mechanic
        from services.dig_constants import get_boss_by_id as _get_boss

        if state_row is None:
            state_row = self.dig_repo.claim_active_duel(discord_id, guild_id)
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
        # Crit was persisted (gear-adjusted, already includes amulet + the
        # risk-tier baseline) when the fight started, so the stored value is
        # authoritative. A zero here means zero — no risk-tier recompute,
        # which would resurrect crit on a fight where it was deliberately 0.
        crit_chance = float(state_row["crit_chance"] or 0.0)
        crit_bonus = int(state_row["crit_bonus"] or 0)

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
                    crit_chance=crit_chance, crit_bonus=crit_bonus,
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
        # Soften UX: best-effort starting HP for resumed fights is the
        # at-pause value (the post-pause portion of the fight is what gets
        # surfaced as soften progress).
        starting_boss_hp_for_resume = int(state_row.get("boss_hp", 0) or 0)
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
            starting_boss_hp=starting_boss_hp_for_resume,
            forced_no_wager_phase=bool(status_effects.get("forced_no_wager_phase")),
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
        crit_chance: float = 0.0, crit_bonus: int = 0,
    ) -> tuple[dict, int, int, bool | None]:
        """Run one auto-round. Returns (entry, player_hp, boss_hp, terminal).

        ``terminal`` is True if player won (boss at 0), False if lost
        (player at 0), None if neither. Mutates ``status_effects`` in-place
        to decrement DOTs and clear one-shot effects.
        """
        entry: dict = {"round": round_num}
        hp_at_round_start = player_hp  # snapshot for Aching Spine regrowth

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
        # Trophy — Weeping Fang: venom chips the boss for the first 4 rounds.
        venom = int(status_effects.get("trophy_venom", 0))
        if venom > 0:
            boss_hp -= 1
            status_effects["trophy_venom"] = venom - 1
            entry["venom"] = True
        if boss_hp <= 0:
            entry["boss_hp"] = 0
            entry["player_hp"] = max(0, player_hp)
            return entry, player_hp, boss_hp, True
        if player_hp <= 0:
            if status_effects.get("relic_deaths_door") and random.random() < 0.40:
                player_hp = 1
                status_effects["relic_deaths_door"] = False
                entry["deaths_door"] = True
                entry["player_hp"] = 1
            else:
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
            crit_this_round = False
            if player_roll:
                dmg_this_round = player_dmg
                if status_effects.get("relic_bottled_quake"):
                    dmg_this_round += 1
                    status_effects["relic_bottled_quake"] = False
                    entry["bottled_quake"] = True
                # Relic — Berserker's Mark: +1 dmg per prior round you took damage (cap +2).
                berserk = status_effects.get("relic_berserk_rage")
                if berserk:
                    dmg_this_round += min(berserk, 2)
                # Trophy — Hateborn Ember: last stand, +1 damage while at 1 HP.
                if player_hp == 1 and status_effects.get("trophy_laststand"):
                    dmg_this_round += 1
                    entry["laststand"] = True
                # Relic — Gambler's Edge: ~10% chance this hit lands double.
                if status_effects.get("relic_double_hit") and random.random() < 0.10:
                    dmg_this_round *= 2
                    entry["double_hit"] = True
                if crit_chance > 0 and random.random() < crit_chance:
                    dmg_this_round += crit_bonus
                    crit_this_round = True
                    if status_effects.get("gear_heal_first_crit"):
                        player_hp = min(
                            int(status_effects.get("trophy_start_hp", player_hp + 1)),
                            player_hp + 1,
                        )
                        status_effects["gear_heal_first_crit"] = False
                        entry["blood_locket_heal"] = True
                boss_hp -= dmg_this_round
                # Trophy — Runebitten Shard: heal 1 on the first landed hit.
                if status_effects.get("trophy_lifesteal"):
                    player_hp += 1
                    status_effects["trophy_lifesteal"] = False
                    entry["lifesteal"] = True
            entry["player_hit"] = player_roll
            entry["crit"] = crit_this_round
            entry["boss_hp"] = max(0, boss_hp)
        else:
            entry["player_hit"] = False
            entry["boss_hp"] = max(0, boss_hp)
            entry["skipped_player"] = True

        if boss_hp <= 0:
            return entry, player_hp, boss_hp, True

        # Boss swing
        if skip != "boss":
            # Trophy — Listening Shard: forewarned, the boss can't land on round 1.
            if round_num == 1 and status_effects.get("trophy_forewarned"):
                boss_roll = False
                entry["forewarned"] = True
            else:
                boss_roll = random.random() < boss_hit
            if boss_roll:
                actual_dmg = boss_dmg + (1 if frost else 0)
                player_hp -= actual_dmg
                if status_effects.get("gear_reflect_first_hit"):
                    boss_hp -= 1
                    status_effects["gear_reflect_first_hit"] = False
                    entry["briarplate_reflect"] = True
            elif status_effects.get("gear_springheel_counter"):
                boss_hp -= 1
                status_effects["gear_springheel_counter"] = False
                entry["springheel_counter"] = True
            entry["boss_hit"] = boss_roll
            entry["player_hp"] = max(0, player_hp)
            entry["boss_hp"] = max(0, boss_hp)
        else:
            entry["boss_hit"] = False
            entry["player_hp"] = max(0, player_hp)
            entry["skipped_boss"] = True

        if boss_hp <= 0:
            return entry, player_hp, boss_hp, True

        # Relic — Berserker's Mark: tally rounds where the player took damage (cap 2).
        if "relic_berserk_rage" in status_effects and player_hp < hp_at_round_start:
            status_effects["relic_berserk_rage"] = min(
                status_effects["relic_berserk_rage"] + 1, 2
            )

        if player_hp <= 0:
            if status_effects.get("relic_deaths_door") and random.random() < 0.40:
                player_hp = 1
                status_effects["relic_deaths_door"] = False
                entry["deaths_door"] = True
                entry["player_hp"] = 1
            else:
                return entry, player_hp, boss_hp, False

        # Trophy — Aching Spine: regrow 1 HP after a round you took no damage,
        # capped at your fight-start HP. Only on a non-terminal round.
        if (
            status_effects.get("trophy_regrowth")
            and player_hp == hp_at_round_start
            and player_hp < int(status_effects.get("trophy_start_hp", player_hp))
        ):
            player_hp += 1
            entry["player_hp"] = max(0, player_hp)
            entry["regrowth"] = True

        return entry, player_hp, boss_hp, None

    def _trophy_status_seed(self, discord_id: int, guild_id, *, player_start_hp: int) -> dict:
        """Build the initial ``status_effects`` carrying equipped trophy- and combat-relic flags.

        These flags persist across a mid-fight prompt pause (``status_effects``
        is serialized into the active-duel row and reloaded on resume) and are
        consumed in :meth:`_run_one_round`.
        """
        se: dict = {}
        if self._has_relic(discord_id, guild_id, "weeping_fang"):
            se["trophy_venom"] = 4
        if self._has_relic(discord_id, guild_id, "runebitten_shard"):
            se["trophy_lifesteal"] = True
        if self._has_relic(discord_id, guild_id, "aching_spine"):
            se["trophy_regrowth"] = True
        if self._has_relic(discord_id, guild_id, "listening_shard"):
            se["trophy_forewarned"] = True
        if self._has_relic(discord_id, guild_id, "hateborn_ember"):
            se["trophy_laststand"] = True
        # Boss-combat relics — not trophies, but seeded + persisted the same way
        # (they survive a mid-fight pause via the serialized status_effects).
        if self._has_relic(discord_id, guild_id, "berserkers_mark"):
            se["relic_berserk_rage"] = 0  # rounds the player has taken damage
        if self._has_relic(discord_id, guild_id, "gamblers_edge"):
            se["relic_double_hit"] = True
        if self._has_relic(discord_id, guild_id, "deaths_door"):
            se["relic_deaths_door"] = True  # one-shot survive-a-killing-blow charge
        if self._has_relic(discord_id, guild_id, "paper_crane"):
            se["relic_paper_crane"] = True
        if self._has_relic(discord_id, guild_id, "bottled_quake"):
            se["relic_bottled_quake"] = True
        gear_effect_flags = {
            "reflect_first_hit": "gear_reflect_first_hit",
            "block_first_status": "gear_block_first_status",
            "springheel_counter": "gear_springheel_counter",
            "block_first_skip": "gear_block_first_skip",
            "heal_first_crit": "gear_heal_first_crit",
        }
        if getattr(self, "dig_repo", None) is not None:
            loadout = self._get_loadout(discord_id, guild_id)
            for piece in (
                loadout.weapon, loadout.armor, loadout.boots, loadout.amulet,
            ):
                if piece is None:
                    continue
                effect_id = getattr(piece.tier_def, "effect_id", None)
                flag = gear_effect_flags.get(effect_id)
                if flag:
                    se[flag] = True
        if se:
            se["trophy_start_hp"] = int(player_start_hp)
        return se

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
        if (
            chosen.skip_next_round_for == "player"
            and new_status.get("gear_block_first_skip")
        ):
            new_status["gear_block_first_skip"] = False
            new_status["anchor_boots_blocked"] = True
        elif chosen.skip_next_round_for:
            new_status["skip_next_round_for"] = chosen.skip_next_round_for
        if chosen.status_effect and new_status.get("relic_paper_crane"):
            new_status["relic_paper_crane"] = False
            new_status["paper_crane_blocked"] = chosen.status_effect
        elif chosen.status_effect and new_status.get("gear_block_first_status"):
            new_status["gear_block_first_status"] = False
            new_status["nullweave_blocked"] = chosen.status_effect
        elif chosen.status_effect and chosen.status_effect in _EFFS:
            # Appliers mutate a state-like dict in the same shape.
            fake_state = {"status_effects": new_status}
            _EFFS[chosen.status_effect](fake_state)
            new_status = fake_state.get("status_effects") or new_status
        return chosen.narrative, player_hp, boss_hp, new_status

    def _apply_shifting_idol_stats(
        self,
        discord_id: int,
        guild_id,
        player_hp: int,
        player_hit: float,
        crit_chance: float,
    ) -> tuple[int, float, float, str | None]:
        """Roll and apply Shifting Idol's fresh bonus for one boss attempt."""
        if not self._has_relic(discord_id, guild_id, "shifting_idol"):
            return player_hp, player_hit, crit_chance, None
        bonus = random.choice(("hp", "hit", "crit"))
        if bonus == "hp":
            player_hp += 1
        elif bonus == "hit":
            player_hit = min(PLAYER_HIT_CEILING, player_hit + 0.05)
        else:
            crit_chance = min(1.0, crit_chance + 0.05)
        return player_hp, player_hit, crit_chance, bonus

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
        starting_boss_hp: int | None = None,
        forced_no_wager_phase: bool = False,
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
                          pre_tick_loadout.boots,
                          pre_tick_loadout.amulet):
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
                phase_def = (
                    get_phase2_for(boss.boss_id if boss else "", at_boss) if needs_phase2
                    else get_phase3_for(boss.boss_id if boss else "", at_boss)
                )
                next_phase_num = 2 if needs_phase2 else 3
                phase_event = random.choice(PHASE_TRANSITION_EVENTS)
                # Mark next phase status, preserving boss_id when present.
                # Drop hp_remaining/hp_max so the next phase starts with its
                # own fresh HP pool (each phase has its own HP).
                if isinstance(current_entry, dict):
                    next_entry = dict(current_entry)
                    next_entry["status"] = next_status
                    next_entry.pop("hp_remaining", None)
                    next_entry.pop("hp_max", None)
                    next_entry["pending_phase_event_id"] = phase_event.id
                    boss_progress[str(at_boss)] = next_entry
                else:
                    boss_progress[str(at_boss)] = {
                        "boss_id": boss.boss_id if boss else "",
                        "status": next_status,
                        "pending_phase_event_id": phase_event.id,
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
            # Honor drain_next_reward curse: -25% on this reward.
            curse_raw = tunnel.get("stinger_curse")
            drain_applied = False
            try:
                curse = json.loads(curse_raw) if curse_raw else {}
            except (json.JSONDecodeError, TypeError):
                curse = {}
            if curse.get("drain_next_reward"):
                drain_applied = True
                curse.pop("drain_next_reward", None)
                # Persist cleared curse flag (keep other curses intact)
                self.dig_repo.update_tunnel(
                    discord_id, guild_id,
                    stinger_curse=(json.dumps(curse) if curse else None),
                )

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
            # Carried wager is settled by the payout below; drop the markers.
            self._clear_carried_wager(boss_progress, at_boss)
            prev_max_depth = tunnel.get("max_depth", 0) or 0
            tunnel_updates = {
                "depth": new_depth,
                "max_depth": max(prev_max_depth, new_depth),
                "boss_progress": json.dumps(boss_progress),
                "boss_attempts": 0,
                "cheer_data": None,
                "last_dig_at": now,
            }
            # Fold the first-clear stat-point award into the atomic victory
            # write (mirroring fight_boss) instead of a separate update_tunnel,
            # so the award and the boss-defeated flip commit together. The award
            # is idempotent: _boss_stat_point_award_updates returns None once the
            # boundary is already in stat_boss_awards, so a retry never re-awards.
            stat_award_updates = self._boss_stat_point_award_updates(tunnel, at_boss)
            stat_point_awarded = stat_award_updates is not None
            if stat_award_updates is not None:
                tunnel_updates.update(stat_award_updates)
            # Every boss victory pays a flat depth-scaled base reward so a
            # win is never empty; a wagered win adds its taper-floored profit
            # on top.
            base_reward = BOSS_VICTORY_BASE_JC.get(at_boss, 15)
            if wager > 0:
                # A won wager never returns less than the stake — the taper
                # plus loot penalties (echo, drain curse) can otherwise drive
                # it negative.
                wager_profit = max(
                    0,
                    int(wager * (multiplier * boss_payout_mult * echo_payout_mult - 1))
                    - (int(round(wager * multiplier * 0.25)) if drain_applied else 0),
                )
            else:
                wager_profit = 0
            net_payout = base_reward + wager_profit
            # Bankruptcy debuff: a penalized player keeps only the configured
            # fraction of the boss-victory winnings; withheld share is a sink.
            net_payout, boss_bankruptcy_penalty = self._penalize_jc(
                discord_id, guild_id, net_payout
            )

            # Tunnel flip + JC payout + boss-echo refresh + audit log all
            # commit in one BEGIN IMMEDIATE. A crash can no longer pay out
            # without clearing the boss (or vice versa).
            self.dig_repo.atomic_boss_full_victory(
                discord_id=discord_id,
                guild_id=guild_id,
                jc_delta=net_payout,
                tunnel_updates=tunnel_updates,
                boss_echo_boss_id=boss.boss_id if boss else "",
                boss_echo_depth=at_boss,
                boss_echo_window_seconds=24 * 3600,
                log_detail={
                    "boundary": at_boss, "won": True, "risk": risk_tier,
                    "wager": wager, "jc_delta": net_payout,
                    "stat_point_awarded": stat_point_awarded,
                    "echo_applied": echo_applied,
                    "rounds": round_log,
                },
            )

            defeat_msg = self._pick_boss_outcome_line(
                boss=boss, boss_name=boss_name, boundary=at_boss, won=True,
            )
            # Boss-drop roll happens once per full kill, NOT on phase-1 transitions.
            gear_drop = self._maybe_drop_gear(discord_id, guild_id, at_boss)
            prestige_relic_drop = self._maybe_drop_prestige_relic(
                discord_id, guild_id, tunnel.get("prestige_level", 0) or 0,
            )
            trophy_relic_drop = self._maybe_carve_trophy_relic(discord_id, guild_id, boss)

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
                jc_delta=net_payout, payout=net_payout,
                bankruptcy_penalty=boss_bankruptcy_penalty,
                new_depth=new_depth,
                dialogue=defeat_msg,
                stat_point_awarded=stat_point_awarded,
                round_log=round_log,
                echo_applied=echo_applied,
                echo_killer_id=(
                    active_echo.get("killer_discord_id")
                    if echo_applied and active_echo else None
                ),
                gear_broken=gear_broken_names,
                gear_drop=gear_drop,
                prestige_relic_drop=prestige_relic_drop,
                trophy_relic_drop=trophy_relic_drop,
                luminosity_display=self._luminosity_combat_display(tunnel),
            )

        # Loss branch
        knockback = random.randint(BOSS_LOSS_KNOCKBACK_MIN, BOSS_LOSS_KNOCKBACK_MAX)
        extra_kb, extra_cd = self._apply_stinger_on_loss(
            discord_id, guild_id, tunnel, boss,
        )
        knockback += extra_kb
        new_depth = max(0, depth - knockback)
        # A loss always costs something when the player elected a wager/free
        # fight. Forced no-wager phase fights should not pay the free-fight
        # repair bill just because wagering is disallowed.
        jc_delta = (
            -wager if wager > 0
            else 0 if forced_no_wager_phase
            else -BOSS_LOSS_REPAIR_BILL
        )
        # Floor the debit at the player's current balance. The wager was only
        # *validated* at start_boss_duel, never escrowed, so a player who
        # spent JC during a mid-fight pause (or whose balance otherwise
        # dropped below the wager) could be driven negative here. Re-read the
        # live balance and clamp the debit to the player's *positive* balance:
        # a loss can never push it below zero, and — crucially — must never
        # CREDIT a player whose balance is already negative (in which case
        # -current_balance would be positive, minting coins on a loss).
        if jc_delta < 0:
            current_balance = self.player_repo.get_balance(discord_id, guild_id)
            jc_delta = max(jc_delta, -max(0, current_balance))
        # Extended cooldown (stinger + flat loss penalty) pushes the timer forward.
        last_dig_effective = now + extra_cd + BOSS_LOSS_EXTRA_COOLDOWN_SECONDS

        # Loss is harsher on gear — an extra durability tick beyond the
        # per-fight tick above, on the same pieces that fought.
        for _ in range(BOSS_LOSS_EXTRA_GEAR_TICKS):
            if gear_snapshot_ids:
                extra_broken = self.dig_repo.tick_gear_durability_ids(
                    [int(g) for g in gear_snapshot_ids]
                )
            else:
                extra_broken = self.dig_repo.tick_gear_durability(discord_id, guild_id)
            gear_broken_names.extend(
                name_by_id.get(i, "a piece of gear") for i in extra_broken
            )

        # Persist remaining boss HP so soften-and-retreat works for the
        # state-machine path. ending_boss_hp / boss_hp_max are forwarded
        # from the caller (start_boss_duel / resume_boss_duel) — when the
        # caller didn't track these (legacy auto-resolve path with no HP
        # info), we skip persistence and the next encounter starts fresh.
        #
        # Persist from the already-consumed ``boss_progress`` ARGUMENT, not a
        # fresh re-read of the in-memory ``tunnel``. start_boss_duel consumes
        # ``pending_phase_event_id`` from boss_progress (and DB) but does NOT
        # refresh tunnel["boss_progress"]; re-reading the stale tunnel here
        # would re-persist the consumed one-shot and re-fire it next fight.
        # The win branch already writes from this same argument.
        bp_for_persist = dict(boss_progress)
        if ending_boss_hp is not None and boss_hp_max is not None:
            self._persist_boss_hp_after_fight(
                bp_for_persist, at_boss, boss.boss_id if boss else "",
                ending_hp=max(0, int(ending_boss_hp)),
                hp_max=max(1, int(boss_hp_max)),
                won=False, outcome="loss", now=int(now),
            )
        # Loss forfeits the carried wager; drop the markers.
        self._clear_carried_wager(bp_for_persist, at_boss)
        # Debit wager and write tunnel state atomically so a crash can't
        # leave depth knocked back without the matching balance change.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=jc_delta,
            tunnel_updates={
                "depth": new_depth,
                "boss_progress": json.dumps(bp_for_persist),
                "boss_attempts": attempts,
                "cheer_data": None,
                "last_dig_at": last_dig_effective,
            },
        )
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
        # Soften progress line: show how much HP the player chipped off
        # before retreating, so the long-grind boss fights feel like progress
        # rather than a flat repeat.
        soften_line = None
        if (
            starting_boss_hp is not None
            and ending_boss_hp is not None
            and boss_hp_max is not None
        ):
            sbp = max(0, int(starting_boss_hp))
            ebp = max(0, int(ending_boss_hp))
            hmax = max(1, int(boss_hp_max))
            chipped = sbp - ebp
            if chipped > 0:
                soften_line = (
                    f"You knocked the boss from {sbp}/{hmax} to {ebp}/{hmax} "
                    f"before retreating."
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
            dialogue=self._pick_boss_outcome_line(
                boss=boss, boss_name=boss_name, boundary=at_boss, won=False,
            ),
            round_log=round_log,
            soften_line=soften_line,
            echo_applied=echo_applied,
            echo_killer_id=(
                active_echo.get("killer_discord_id")
                if echo_applied and active_echo else None
            ),
            gear_broken=gear_broken_names,
            gear_drop=None,
            luminosity_display=self._luminosity_combat_display(tunnel),
        )

    def retreat_boss(self, discord_id: int, guild_id) -> dict:
        """Retreat from boss. Lose 2-3 blocks.

        Persisted boss HP from any prior engagement is preserved (the
        retreat exchanges no blows). Retreating from a phase-2/3 encounter
        forfeits half of the carried wager.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")

        tunnel = dict(tunnel)
        boss_progress = self._get_boss_progress_entries(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)

        if at_boss is None:
            return self._error("You're not at a boss boundary.")

        loss = random.randint(RETREAT_BLOCK_LOSS_MIN, RETREAT_BLOCK_LOSS_MAX)
        new_depth = max(0, depth - loss)

        # Multi-phase carry: retreating from a phase-2/3 encounter forfeits
        # half of the carried wager. Pure phase-1 retreat (no carry) keeps
        # the existing behavior of "no JC at risk".
        carried = self._get_carried_wager(boss_progress, at_boss)
        carried_forfeit = 0
        if carried is not None and self._is_pinnacle_depth(at_boss):
            carried_wager_amount, _ = carried
            carried_forfeit = carried_wager_amount // 2

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
        # Drop the carry markers so the next encounter starts fresh.
        self._clear_carried_wager(boss_progress, at_boss)

        # Tunnel mutation + carry-forfeit debit + audit log commit together;
        # without the atomic helper a crash between the boss_progress wipe
        # and the JC debit would let the player keep both phase-1 progress
        # and the full carried wager.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-carried_forfeit if carried_forfeit > 0 else 0,
            tunnel_updates={
                "depth": new_depth,
                "boss_progress": json.dumps(boss_progress),
            },
            log_detail={
                "boundary": at_boss, "loss": loss,
                "carried_wager_forfeit": carried_forfeit,
            },
            log_action_type="boss_retreat",
        )

        return self._ok(
            boundary=at_boss,
            loss=loss,
            new_depth=new_depth,
            carried_wager_forfeit=carried_forfeit,
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
            _scout_crit_chance = float(stats.get("crit_chance", 0) or 0)
            _scout_crit_bonus = int(stats.get("crit_bonus", 0) or 0)
            win_pct = dig_service._approx_duel_win_prob(
                player_hp=int(stats["player_hp"]),
                boss_hp=boss_hp,
                player_hit=player_hit,
                player_dmg=int(stats["player_dmg"]),
                boss_hit=boss_hit_chance,
                boss_dmg=boss_dmg_eff,
                crit_chance=_scout_crit_chance,
                crit_bonus=_scout_crit_bonus,
            )
            free_win_pct = dig_service._approx_duel_win_prob(
                player_hp=int(stats["player_hp"]),
                boss_hp=boss_hp,
                player_hit=free_hit,
                player_dmg=int(stats["player_dmg"]),
                boss_hit=boss_hit_chance,
                boss_dmg=boss_dmg_eff,
                crit_chance=_scout_crit_chance,
                crit_bonus=_scout_crit_bonus,
            )
            base_multiplier = payouts[i] if i < len(payouts) else 2.0
            odds[tier] = {
                "win_pct": round(win_pct, 2),
                "free_fight_pct": round(free_win_pct, 2),
                "player_hp": int(stats["player_hp"]),
                "boss_hp": boss_hp,
                "player_hit": round(player_hit, 2),
                "boss_hit": round(boss_hit_chance, 2),
                "multiplier": round(
                    self._effective_wager_multiplier(base_multiplier, win_pct)
                    * payout_mult, 2,
                ),
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
        """Cheer for a player fighting a boss. Free; capped at 3 per fight."""
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

        cost = 0

        # Check max cheers (3 max = +15%) — global per-fight cap.
        cheers = self._get_cheers(target_tunnel)
        now = int(time.time())
        active_cheers = [c for c in cheers if c.get("expires_at", 0) > now]
        if len(active_cheers) >= 3:
            return self._error("Boss already at full cheer boost (3/3).")

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
