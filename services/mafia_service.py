"""Daily Mafia subgame service.

Encapsulates roster collection, role assignment, phase resolution, payouts,
and read-side queries for the /mafia command surface.
"""

import logging
import random
import time
from collections import Counter

from config import MAX_DEBT
from domain.models.mafia import (
    MAFIA_ROLES,
    TOWN_ROLES,
    MafiaActionType,
    MafiaGame,
    MafiaPhase,
    MafiaPlayer,
    MafiaRole,
    MafiaTwist,
    MafiaWinner,
)

logger = logging.getLogger("cama_bot.services.mafia")

# Roster size → role count distribution. Remainder of the roster fills as TOWNIE.
ROLE_TABLE: dict[int, dict[str, int]] = {
    5:  {"mafia": 1, "doctor": 1, "detective": 1, "vigilante": 0},
    6:  {"mafia": 1, "doctor": 1, "detective": 1, "vigilante": 0},
    7:  {"mafia": 2, "doctor": 1, "detective": 1, "vigilante": 0},
    8:  {"mafia": 2, "doctor": 1, "detective": 1, "vigilante": 0},
    9:  {"mafia": 2, "doctor": 1, "detective": 1, "vigilante": 0},
    10: {"mafia": 3, "doctor": 1, "detective": 1, "vigilante": 1},
    11: {"mafia": 3, "doctor": 1, "detective": 1, "vigilante": 1},
    12: {"mafia": 3, "doctor": 1, "detective": 1, "vigilante": 1},
    13: {"mafia": 4, "doctor": 1, "detective": 1, "vigilante": 1},
    14: {"mafia": 4, "doctor": 1, "detective": 1, "vigilante": 1},
    15: {"mafia": 4, "doctor": 1, "detective": 1, "vigilante": 1},
}

MIN_ROSTER = 5
MAX_ROSTER = 15

# Phases advance as soon as every living actor has acted (see night_ready /
# day_ready). These durations are only an anti-AFK *fallback*: if someone never
# acts, the phase force-advances after this long so a game can't stall forever.
NIGHT_DURATION_S = 24 * 3600
DAY_DURATION_S = 24 * 3600
# Ping un-acted players if a phase has been open this long without resolving.
PHASE_REMINDER_AFTER_S = 8 * 3600
# Absolute backstop on cycles so a degenerate game can't loop forever.
MAX_CYCLES = 14

# Probabilities.
JESTER_PROBABILITY = 0.20
BOOKIE_PROBABILITY = 0.20
TWIST_PROBABILITY = 0.30

# Eligibility window for /gamba and /dig activity feeding the auto-roster.
ELIGIBILITY_WINDOW_S = 7 * 24 * 3600

# Auto-skip: if a player's last N games all show acted=0, exclude from auto-roster.
AUTO_SKIP_THRESHOLD = 3

# Economy. Each rostered player is charged ENTRY_FEE at game start; the fees
# pool into a single pot distributed among the winning faction at day
# resolution. Because roles are assigned uniformly at random, the per-player
# expectation of the pot share equals exactly ENTRY_FEE regardless of the
# (unknown) faction win-rate distribution — so EV per game = 0 by construction.
# MVP_BONUS is now drawn from the pot rather than minted.
ENTRY_FEE = 8
MVP_BONUS = 20
# Hard cap on what any single winner can take from one game's pot. Whatever the
# pot would have paid a winner beyond this overflows to the nonprofit fund, so
# the game stays zero-sum (coins move to a pool, never minted or burned).
MAX_WINNER_PAYOUT = 50
# The Bookie (neutral) skims this much off the top when their lynch wager hits.
BOOKIE_PAYOUT = MAX_WINNER_PAYOUT

TITLES: dict[str, callable] = {
    "Don of Dire":  lambda s: s["mafia_wins"] >= 10,
    "Inquisitor":   lambda s: s["correct_reads"] >= 5,
    "Reaper":       lambda s: s["mafia_kills"] >= 5,
    "Lifesaver":    lambda s: s["doctor_saves"] >= 5,
    "Wildcard":     lambda s: s["jester_wins"] >= 1,
}


def _pot_for_roster(roster_size: int) -> int:
    return roster_size * ENTRY_FEE


def phase_duration(phase: MafiaPhase, at_ts: int) -> int:
    """Anti-AFK fallback length for a NIGHT/DAY phase (force-advance backstop).

    Phases normally resolve the moment everyone has acted; this is just the
    maximum a phase stays open if someone never does. ``at_ts`` is accepted for
    signature stability (the duration is uniform now).
    """
    return NIGHT_DURATION_S if phase == MafiaPhase.NIGHT else DAY_DURATION_S


def _allowed_action_for_role(role: MafiaRole) -> MafiaActionType | None:
    return {
        MafiaRole.MAFIA: MafiaActionType.KILL,
        MafiaRole.DOCTOR: MafiaActionType.SAVE,
        MafiaRole.DETECTIVE: MafiaActionType.INVESTIGATE,
        MafiaRole.VIGILANTE: MafiaActionType.VIG_KILL,
        MafiaRole.BOOKIE: MafiaActionType.WAGER,
    }.get(role)


class MafiaService:
    """Business logic for the Daily Mafia subgame."""

    def __init__(
        self,
        mafia_repo,
        player_repo,
        dig_service,
        flavor_service,
        hero_provider,
        rng: random.Random | None = None,
        max_debt: int = MAX_DEBT,
        bankruptcy_penalty_rate: float | None = None,
    ):
        self.repo = mafia_repo
        self.player_repo = player_repo
        self.dig_service = dig_service
        self.flavor_service = flavor_service
        self.hero_provider = hero_provider
        self._rng = rng or random.Random()
        self.max_debt = max_debt
        self.bankruptcy_penalty_rate = bankruptcy_penalty_rate

    # ────────────────────────────────────────────────────────────────────
    # Lifecycle
    # ────────────────────────────────────────────────────────────────────

    def start_game(
        self, guild_id: int | None, *, force: bool = False
    ) -> MafiaGame | None:
        """Idempotent. Start a new game if none is currently active.

        Games run back-to-back with no calendar gating: the cog calls this on
        every tick where there's no active game and immediately after one
        resolves. ``game_date`` records the start date (informational; it may
        repeat). Returns the new/existing ACTIVE game, or None if the roster is
        too small. ``force`` is accepted for the admin path (kept for clarity).
        """
        today = self.dig_service._get_game_date()

        active = self.repo.get_active_game(guild_id)
        if active is not None and active.status == "ACTIVE":
            return active

        signups = self.repo.get_signups(guild_id)
        for attempt in range(2):
            eligible = self._collect_roster(guild_id, signups=signups)
            if len(eligible) < MIN_ROSTER:
                logger.info(
                    "Mafia start skipped for guild %s: only %d eligible players",
                    guild_id,
                    len(eligible),
                )
                return None

            roster_size = len(eligible)
            twist = self._roll_twist()
            started_at = int(time.time())
            players = self._assign_roles(0, guild_id, eligible)

            try:
                game_id = self.repo.create_game_with_players_and_entry_fees(
                    guild_id=guild_id,
                    game_date=today,
                    phase=MafiaPhase.NIGHT,
                    started_at=started_at,
                    roster_size=roster_size,
                    twist_event=twist,
                    entry_fee=ENTRY_FEE,
                    players=players,
                    max_debt=self.max_debt,
                )
            except ValueError as exc:
                if str(exc) == "entry_fee_debit_failed" and attempt == 0:
                    continue
                logger.info(
                    "Mafia start skipped for guild %s: entry fee debit failed",
                    guild_id,
                )
                return None

            self.repo.clear_signups(guild_id)
            game = self.repo.get_game_by_id(game_id)
            if game is not None:
                game.players = self.repo.get_players(game_id)
            return game

        return None

    def _collect_roster(
        self, guild_id: int | None, *, signups: list[int] | tuple[int, ...] = ()
    ) -> list[int]:
        """Eligible players capped at MAX_ROSTER, opt-in signups first."""
        eligible = self._collect_eligible_players(guild_id)
        if len(eligible) <= MAX_ROSTER:
            return eligible
        signup_set = set(signups)
        prioritized = [pid for pid in eligible if pid in signup_set]
        rest = [pid for pid in eligible if pid not in signup_set]
        self._rng.shuffle(rest)
        return (prioritized + rest)[:MAX_ROSTER]

    def resolve_night(self, guild_id: int | None) -> dict:
        """Apply night actions, mark the dead, advance to DAY.

        Returns a summary dict for the cog to render publicly.
        """
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase != MafiaPhase.NIGHT:
            return {"resolved": False, "reason": "no_active_night"}

        dn = game.day_number
        all_players = self.repo.get_players(game.game_id)
        by_id = {p.discord_id: p for p in all_players}
        alive_ids = {p.discord_id for p in all_players if p.is_alive}
        mafia_ids = {p.discord_id for p in all_players if p.role == MafiaRole.MAFIA}

        kill_actions = self.repo.get_actions(
            game.game_id, MafiaActionType.KILL, MafiaPhase.NIGHT, day_number=dn
        )
        save_actions = self.repo.get_actions(
            game.game_id, MafiaActionType.SAVE, MafiaPhase.NIGHT, day_number=dn
        )
        vig_actions = self.repo.get_actions(
            game.game_id, MafiaActionType.VIG_KILL, MafiaPhase.NIGHT, day_number=dn
        )

        # Determine mafia kill targets.
        mafia_kill_targets = self._tally_mafia_kill(
            kill_actions, alive_ids, mafia_ids, twist=game.twist_event
        )

        # Doctor save: blocks ONE mafia kill matching the saved target.
        save_target = save_actions[0]["target_id"] if save_actions else None
        if save_target is not None and save_target in mafia_kill_targets:
            mafia_kill_targets.remove(save_target)

        # Vigilante kill: independent of mafia, not blockable by doctor.
        vig_target = None
        if vig_actions:
            vt = vig_actions[0]["target_id"]
            if vt in alive_ids and vt not in mafia_kill_targets:
                vig_target = vt

        # Plague: extra unblockable death on a random alive non-already-targeted player.
        plague_target = None
        if game.twist_event == MafiaTwist.PLAGUE:
            already_dying = set(mafia_kill_targets)
            if vig_target is not None:
                already_dying.add(vig_target)
            candidates = [pid for pid in alive_ids if pid not in already_dying]
            if candidates:
                plague_target = self._rng.choice(candidates)

        killed: list[dict] = []
        killed_ids: list[int] = []
        for tid in mafia_kill_targets:
            killed.append({"discord_id": tid, "by": "mafia"})
            killed_ids.append(tid)
        if vig_target is not None:
            killed.append({"discord_id": vig_target, "by": "vigilante"})
            killed_ids.append(vig_target)
        if plague_target is not None:
            killed.append({"discord_id": plague_target, "by": "plague"})
            killed_ids.append(plague_target)

        now = int(time.time())
        applied = self.repo.apply_night_resolution(
            game.game_id, killed_ids, ended_at=now
        )
        if not applied:
            return {"resolved": False, "reason": "night_already_resolved"}

        # Process detective investigations only after the night has actually
        # resolved. Recording them earlier let a concurrent no-op tick (one that
        # loses the apply_night_resolution race) still re-write detective rows.
        if game.twist_event != MafiaTwist.MEMORY_FOG:
            self._record_detective_results(game, by_id)

        # Resurrection (weekend-only, once per game): revive a fallen non-mafia
        # at dawn. Clearing the twist makes it idempotent.
        revived_id = self._maybe_resurrect(game, killed_ids, now)

        return {
            "resolved": True,
            "game_id": game.game_id,
            "day_number": dn,
            "twist": game.twist_event.value if game.twist_event else None,
            "killed": killed,
            "revived_id": revived_id,
            "save_blocked": save_target is not None and save_target not in [k["discord_id"] for k in killed],
        }

    def _maybe_resurrect(
        self, game: MafiaGame, killed_ids: list[int], now: int
    ) -> int | None:
        """Weekend Resurrection event: revive one dead non-mafia, once per game."""
        from utils.game_date import game_date_for_timestamp, weekday_of_game_date

        if game.twist_event != MafiaTwist.RESURRECTION:
            return None
        if weekday_of_game_date(game_date_for_timestamp(now)) < 5:  # weekday
            return None
        players = self.repo.get_players(game.game_id)
        just_killed = set(killed_ids)
        candidates = [
            p.discord_id
            for p in players
            if not p.is_alive
            and p.role != MafiaRole.MAFIA
            and p.discord_id not in just_killed
        ]
        if not candidates:
            return None
        revived = self._rng.choice(candidates)
        self.repo.revive_player(game.game_id, revived)
        # Consume the event so it can't fire again.
        self.repo.set_twist_event(game.game_id, None)
        return revived

    def resolve_day(self, guild_id: int | None) -> dict:
        """Resolve a day: lynch, bounties, win check; then continue or finalize."""
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase != MafiaPhase.DAY:
            return {"resolved": False, "reason": "no_active_day"}

        dn = game.day_number
        all_players = self.repo.get_players(game.game_id)
        by_id = {p.discord_id: p for p in all_players}
        alive_ids = {p.discord_id for p in all_players if p.is_alive}

        votes = self.repo.get_actions(
            game.game_id, MafiaActionType.VOTE, MafiaPhase.DAY, day_number=dn
        )
        valid_votes = [
            v for v in votes
            if v["actor_id"] in alive_ids and v["target_id"] in alive_ids
        ]

        # Town Hall only suppresses the lynch on the first day (a week-long
        # suppression would make the game unwinnable by vote).
        town_hall_today = game.twist_event == MafiaTwist.TOWN_HALL and dn == 1
        lynched_id = None if town_hall_today else self._tally_lynch(valid_votes)

        if lynched_id is not None and lynched_id in by_id:
            by_id[lynched_id].is_alive = False
            by_id[lynched_id].eliminated_phase = MafiaPhase.DAY

        post_players = list(by_id.values())
        post_alive = [p for p in post_players if p.is_alive]

        lynched_was_mafia = (
            lynched_id is not None and by_id[lynched_id].role == MafiaRole.MAFIA
        )

        def _commit_post_claim() -> dict:
            # Both durable side effects fire ONLY after the day resolution is
            # atomically claimed below: the Bookie HIT mark and the Town Bounty
            # fund-draw. Doing either earlier let a lost race with an admin
            # stop/abort leave a spurious HIT on record (which a rival finalize
            # would pay a skim for) or debit the nonprofit fund (and re-pay,
            # since bounties aren't marked) for a day that then reports
            # unresolved. The current day's hit still feeds THIS day's skim via
            # _bookie_hit_today (in memory) before finalize, so same-day cash-out
            # is unaffected.
            self._mark_bookie_wager(game, dn, lynched_id)
            return self.repo.resolve_day_bounties(
                game_id=game.game_id,
                guild_id=guild_id,
                day_number=dn,
                lynched_id=lynched_id,
                lynched_was_mafia=lynched_was_mafia,
                alive_count=len(post_alive),
            )

        # Win evaluation.
        jester_win = (
            lynched_id is not None and by_id[lynched_id].role == MafiaRole.JESTER
        )
        alive_mafia = sum(1 for p in post_alive if p.role == MafiaRole.MAFIA)
        alive_non_mafia = sum(1 for p in post_alive if p.role != MafiaRole.MAFIA)
        cap_reached = self._cycle_cap_reached(dn)

        winner: MafiaWinner | None
        if jester_win:
            winner = MafiaWinner.JESTER
        elif alive_mafia == 0:
            winner = MafiaWinner.TOWN
        elif alive_mafia >= alive_non_mafia:
            winner = MafiaWinner.MAFIA
        elif cap_reached:
            # Hit the cycle cap: force a standing tally — town survived if it
            # still outnumbers the mafia.
            winner = (
                MafiaWinner.TOWN if alive_non_mafia > alive_mafia else MafiaWinner.MAFIA
            )
        else:
            winner = None  # undecided → the game continues to the next cycle

        if winner is None:
            # Apply the lynch death and roll into the next night. No payout.
            if lynched_id is not None:
                self.repo.set_player_alive(
                    game.game_id, lynched_id, alive=False,
                    eliminated_phase=MafiaPhase.DAY,
                )
            advanced = self.repo.advance_to_next_cycle(
                game.game_id, ended_at=int(time.time())
            )
            if not advanced:
                return {"resolved": False, "reason": "day_already_resolved"}
            bounty = _commit_post_claim()
            return {
                "resolved": True,
                "continued": True,
                "game_id": game.game_id,
                "day_number": dn,
                "lynched_id": lynched_id,
                "alive_count": len(post_alive),
                "bounty": bounty,
                "vote_breakdown": self._vote_breakdown(valid_votes),
                "vote_detail": self._vote_detail(valid_votes),
                "twist": game.twist_event.value if game.twist_event else None,
            }

        # Finalize: capped payouts, nonprofit overflow, end-of-week Bookie skim.
        entry_fee = game.entry_fee or ENTRY_FEE
        pot_total = game.roster_size * entry_fee
        winning_ids = self._winners_for(post_players, winner)
        mvp_id = self._compute_mvp(game, post_players, winner, lynched_id, valid_votes)
        bookie_id = self._bookie_hits_over_week(
            game, post_players, extra_hit=self._bookie_hit_today(game, dn, lynched_id)
        )

        deltas, payout_per_winner, nonprofit_overflow = self._compute_payout_deltas(
            pot_total, winning_ids, mvp_id, bookie_id
        )

        finalize = self.repo.finalize_day_resolution(
            game_id=game.game_id,
            winner=winner,
            payout_per_winner=payout_per_winner,
            mvp_id=mvp_id,
            lynched_id=lynched_id,
            payout_deltas=deltas,
            entry_fee=entry_fee,
            bankruptcy_penalty_rate=self.bankruptcy_penalty_rate,
            nonprofit_overflow=nonprofit_overflow,
        )
        if not finalize.get("applied"):
            return {
                "resolved": False,
                "reason": finalize.get("reason", "day_already_resolved"),
            }
        bounty = _commit_post_claim()

        return {
            "resolved": True,
            "continued": False,
            "game_id": game.game_id,
            "day_number": dn,
            "winner": winner.value,
            "lynched_id": lynched_id,
            "mvp_id": mvp_id,
            # Bonus actually awarded to the MVP (clamped to the faction pot), so
            # the embed can report the real amount instead of a hardcoded MVP_BONUS.
            "mvp_bonus": (deltas.get(mvp_id, 0) - payout_per_winner)
            if (mvp_id is not None and mvp_id in winning_ids)
            else 0,
            "payout_per_winner": payout_per_winner,
            "pot_total": pot_total,
            "entry_fee": entry_fee,
            "winning_ids": list(winning_ids),
            "bookie_id": bookie_id,
            "bookie_payout": deltas.get(bookie_id, 0) if bookie_id is not None else 0,
            "nonprofit_overflow": nonprofit_overflow,
            "bounty": bounty,
            "cap_forced": cap_reached and not jester_win and alive_mafia > 0,
            "bankruptcy_penalties": finalize.get("bankruptcy_penalties", {}),
            "vote_breakdown": self._vote_breakdown(valid_votes),
            "vote_detail": self._vote_detail(valid_votes),
            "twist": game.twist_event.value if game.twist_event else None,
        }

    def _compute_payout_deltas(
        self,
        pot_total: int,
        winning_ids: list[int],
        mvp_id: int | None,
        bookie_id: int | None,
    ) -> tuple[dict[int, int], int, int]:
        """Build per-winner payout deltas: Bookie skim off the top, faction split
        with MVP bonus + dust, each capped at +50, remainder → nonprofit overflow.
        Returns (deltas, payout_per_winner, nonprofit_overflow).
        """
        deltas: dict[int, int] = {}
        faction_pot = pot_total
        if bookie_id is not None:
            bookie_take = min(BOOKIE_PAYOUT, pot_total)
            deltas[bookie_id] = bookie_take
            faction_pot = pot_total - bookie_take

        if winning_ids:
            mvp_in_winners = mvp_id is not None and mvp_id in winning_ids
            # Clamp the MVP bonus to what the faction pot can cover so a small
            # pot (e.g. mostly consumed by the Bookie skim) can never push the
            # base split negative — that would mint coins on resolution.
            mvp_share = min(MVP_BONUS, faction_pot) if mvp_in_winners else 0
            remainder_pot = faction_pot - mvp_share
            base_payout = remainder_pot // len(winning_ids)
            dust = remainder_pot - base_payout * len(winning_ids)
            for wid in winning_ids:
                deltas[wid] = base_payout
            if mvp_in_winners:
                deltas[mvp_id] += mvp_share + dust
            elif dust:
                deltas[winning_ids[0]] += dust
            payout_per_winner = base_payout
        else:
            payout_per_winner = 0

        deltas = {wid: min(amount, MAX_WINNER_PAYOUT) for wid, amount in deltas.items()}
        nonprofit_overflow = pot_total - sum(deltas.values())
        payout_per_winner = min(payout_per_winner, MAX_WINNER_PAYOUT)
        return deltas, payout_per_winner, nonprofit_overflow

    def force_finalize(self, guild_id: int | None) -> dict:
        """Admin 'stop': end the game now with a standing tally + payout."""
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase == MafiaPhase.RESOLVED or game.status != "ACTIVE":
            return {"resolved": False, "reason": "no_active_game"}

        players = self.repo.get_players(game.game_id)
        alive = [p for p in players if p.is_alive]
        alive_mafia = sum(1 for p in alive if p.role == MafiaRole.MAFIA)
        alive_non_mafia = sum(1 for p in alive if p.role != MafiaRole.MAFIA)
        if alive_mafia == 0:
            winner = MafiaWinner.TOWN
        elif alive_mafia >= alive_non_mafia:
            winner = MafiaWinner.MAFIA
        else:
            winner = (
                MafiaWinner.TOWN if alive_non_mafia > alive_mafia else MafiaWinner.MAFIA
            )

        entry_fee = game.entry_fee or ENTRY_FEE
        pot_total = game.roster_size * entry_fee
        winning_ids = self._winners_for(players, winner)
        mvp_id = self._compute_mvp(game, players, winner, None, [])
        bookie_id = self._bookie_hits_over_week(game, players)
        deltas, payout_per_winner, nonprofit_overflow = self._compute_payout_deltas(
            pot_total, winning_ids, mvp_id, bookie_id
        )
        # finalize_day_resolution only fires from the DAY phase.
        self.repo.set_phase(game.game_id, MafiaPhase.DAY)
        finalize = self.repo.finalize_day_resolution(
            game_id=game.game_id,
            winner=winner,
            payout_per_winner=payout_per_winner,
            mvp_id=mvp_id,
            lynched_id=None,
            payout_deltas=deltas,
            entry_fee=entry_fee,
            bankruptcy_penalty_rate=self.bankruptcy_penalty_rate,
            nonprofit_overflow=nonprofit_overflow,
        )
        if not finalize.get("applied"):
            return {"resolved": False, "reason": finalize.get("reason", "finalize_failed")}
        return {
            "resolved": True,
            "continued": False,
            "game_id": game.game_id,
            "day_number": game.day_number,
            "winner": winner.value,
            "lynched_id": None,
            "mvp_id": mvp_id,
            "mvp_bonus": (deltas.get(mvp_id, 0) - payout_per_winner)
            if (mvp_id is not None and mvp_id in winning_ids)
            else 0,
            "payout_per_winner": payout_per_winner,
            "pot_total": pot_total,
            "entry_fee": entry_fee,
            "winning_ids": list(winning_ids),
            "bookie_id": bookie_id,
            "bookie_payout": deltas.get(bookie_id, 0) if bookie_id is not None else 0,
            "nonprofit_overflow": nonprofit_overflow,
            "forced_stop": True,
            "vote_breakdown": {},
            "twist": game.twist_event.value if game.twist_event else None,
        }

    def abort_game(self, guild_id: int | None, *, refund: bool = True) -> dict:
        """Admin 'abort': cancel the game, optionally refunding entry fees."""
        game = self.repo.get_active_game(guild_id)
        if game is None or game.status != "ACTIVE":
            return {"ok": False, "reason": "no_active_game"}
        result = self.repo.cancel_game(game.game_id, refund=refund)
        return {
            "ok": result.get("applied", False),
            "game_id": game.game_id,
            "refunded": result.get("refunded", {}),
            "standings_message_id": game.standings_message_id,
        }

    def _cycle_cap_reached(self, dn: int) -> bool:
        """Absolute backstop — force a standing tally past MAX_CYCLES cycles."""
        return dn >= MAX_CYCLES

    def _mark_bookie_wager(
        self, game: MafiaGame, dn: int, lynched_id: int | None
    ) -> None:
        """Durably flag a correct Bookie wager for this day (result='HIT')."""
        if lynched_id is None:
            return
        wagers = self.repo.get_actions(
            game.game_id, MafiaActionType.WAGER, MafiaPhase.NIGHT, day_number=dn
        )
        for w in wagers:
            if w["target_id"] == lynched_id and w.get("result") != "HIT":
                self.repo.record_action(
                    game_id=game.game_id,
                    guild_id=game.guild_id,
                    actor_id=w["actor_id"],
                    target_id=lynched_id,
                    action_type=MafiaActionType.WAGER,
                    phase=MafiaPhase.NIGHT,
                    result="HIT",
                    day_number=dn,
                )

    # ────────────────────────────────────────────────────────────────────
    # Action submission
    # ────────────────────────────────────────────────────────────────────

    def submit_night_action(
        self,
        guild_id: int | None,
        actor_id: int,
        target_id: int,
        action_type: MafiaActionType,
    ) -> dict:
        """Validate and record a NIGHT action. Returns a status dict."""
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase != MafiaPhase.NIGHT:
            return {"ok": False, "error": "Night phase is not active."}

        actor = self.repo.get_player(game.game_id, actor_id)
        if actor is None:
            return {"ok": False, "error": "You're not in today's game."}
        if not actor.is_alive:
            return {"ok": False, "error": "You're dead. Rest in peace."}

        expected = _allowed_action_for_role(actor.role)
        if expected is None or expected != action_type:
            return {"ok": False, "error": "Your role can't perform that action."}

        target = self.repo.get_player(game.game_id, target_id)
        if target is None or not target.is_alive:
            return {"ok": False, "error": "Target is not a living player."}

        # Per-role validations.
        if action_type == MafiaActionType.KILL and target.role == MafiaRole.MAFIA:
            return {"ok": False, "error": "Mafia don't kill their own."}
        if action_type == MafiaActionType.INVESTIGATE and target_id == actor_id:
            return {"ok": False, "error": "You can't investigate yourself."}
        if action_type == MafiaActionType.INVESTIGATE:
            # One read per night (scoped to this cycle). Re-checking the same
            # target replays the cached verdict; a different target is rejected
            # so the detective can't read the whole town from instant replies.
            prior = self.repo.get_action_for_actor(
                game.game_id, actor_id, MafiaActionType.INVESTIGATE,
                day_number=game.day_number,
            )
            if prior is not None:
                if prior["target_id"] == target_id:
                    return {
                        "ok": True,
                        "action": action_type.value,
                        "result": prior["result"],
                    }
                return {
                    "ok": False,
                    "error": "You've already used your read tonight — "
                    "it's locked on your first target.",
                }
        if action_type == MafiaActionType.VIG_KILL:
            prior = self.repo.get_action_for_actor(
                game.game_id, actor_id, MafiaActionType.VIG_KILL
            )
            if prior is not None:
                return {"ok": False, "error": "You've already used your one shot."}

        # Detective: compute and persist the result immediately.
        result_payload: str | None = None
        if action_type == MafiaActionType.INVESTIGATE:
            if game.twist_event == MafiaTwist.MEMORY_FOG:
                result_payload = "Town"  # fog: detective sees nothing useful; default to Town
            elif target.is_godfather:
                result_payload = "Town"
            elif target.role == MafiaRole.MAFIA:
                result_payload = "Mafia"
            else:
                result_payload = "Town"

        self.repo.record_action(
            game_id=game.game_id,
            guild_id=guild_id,
            actor_id=actor_id,
            target_id=target_id,
            action_type=action_type,
            phase=MafiaPhase.NIGHT,
            result=result_payload,
            day_number=game.day_number,
        )

        out: dict = {"ok": True, "action": action_type.value}
        if action_type == MafiaActionType.INVESTIGATE:
            out["result"] = result_payload
        return out

    def submit_day_vote(
        self, guild_id: int | None, voter_id: int, target_id: int
    ) -> dict:
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase != MafiaPhase.DAY:
            return {"ok": False, "error": "Day phase is not active."}

        voter = self.repo.get_player(game.game_id, voter_id)
        if voter is None:
            return {"ok": False, "error": "You're not in today's game."}
        if not voter.is_alive:
            return {"ok": False, "error": "Dead players don't vote."}

        target = self.repo.get_player(game.game_id, target_id)
        if target is None or not target.is_alive:
            return {"ok": False, "error": "Target is not a living player."}

        prior = self.repo.get_action_for_actor(
            game.game_id, voter_id, MafiaActionType.VOTE, day_number=game.day_number
        )
        changed = prior is not None and prior["target_id"] != target_id
        self.repo.record_action(
            game_id=game.game_id,
            guild_id=guild_id,
            actor_id=voter_id,
            target_id=target_id,
            action_type=MafiaActionType.VOTE,
            phase=MafiaPhase.DAY,
            day_number=game.day_number,
        )
        return {"ok": True, "changed": changed}

    def submit_bounty(
        self, guild_id: int | None, contributor_id: int, target_id: int
    ) -> dict:
        """Stake 1 JC on a suspect during the day (Town Bounty)."""
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase != MafiaPhase.DAY:
            return {"ok": False, "error": "Bounties can only be placed during the day."}
        contributor = self.repo.get_player(game.game_id, contributor_id)
        if contributor is None or not contributor.is_alive:
            return {"ok": False, "error": "Only living players can place bounties."}
        if target_id == contributor_id:
            return {"ok": False, "error": "You can't put a bounty on yourself."}
        target = self.repo.get_player(game.game_id, target_id)
        if target is None or not target.is_alive:
            return {"ok": False, "error": "Target is not a living player."}
        res = self.repo.add_bounty(
            game_id=game.game_id,
            guild_id=guild_id,
            day_number=game.day_number,
            target_id=target_id,
            contributor_id=contributor_id,
            max_debt=self.max_debt,
        )
        if not res.get("ok"):
            msg = {
                "already_staked": "You've already staked your jopacoin on them today.",
                "insufficient_funds": "You can't cover the 1 jopacoin stake.",
            }.get(res.get("error"), "Bounty rejected.")
            return {"ok": False, "error": msg}
        return {"ok": True}

    def join(self, guild_id: int | None, discord_id: int) -> dict:
        """Reserve a roster seat in the next game (opt-in priority)."""
        self.repo.add_signup(guild_id, discord_id)
        # Opting in also clears any standing opt-out.
        self.repo.set_optout(guild_id, discord_id, False)
        return {"ok": True}

    def night_ready(self, game: MafiaGame) -> bool:
        """True once every living night-actor has submitted this cycle."""
        return not self.players_needing_night_action(game)

    def day_ready(self, game: MafiaGame) -> bool:
        """True once every living player has voted this cycle."""
        return not self.players_needing_day_vote(game)

    # ────────────────────────────────────────────────────────────────────
    # Read-side
    # ────────────────────────────────────────────────────────────────────

    def get_public_status(self, guild_id: int | None) -> dict:
        game = self.repo.get_active_game(guild_id)
        if game is None:
            return {"active": False}

        players = self.repo.get_players(game.game_id)
        alive = [p for p in players if p.is_alive]
        deaths = [p for p in players if not p.is_alive]

        phase_start = game.phase_started_at or game.started_at
        out = {
            "active": True,
            "game_id": game.game_id,
            "phase": game.phase.value,
            "started_at": game.started_at,
            "day_number": game.day_number,
            "alive_count": len(alive),
            "roster_size": game.roster_size,
            "deaths": [
                {
                    "discord_id": p.discord_id,
                    "role": p.role.value,
                    "phase": p.eliminated_phase.value if p.eliminated_phase else None,
                }
                for p in deaths
            ],
            "twist": game.twist_event.value if game.twist_event else None,
        }

        if game.phase in (MafiaPhase.NIGHT, MafiaPhase.DAY):
            out["phase_ends_at"] = phase_start + phase_duration(game.phase, phase_start)
        if game.phase == MafiaPhase.DAY:
            votes = self.repo.get_actions(
                game.game_id, MafiaActionType.VOTE, MafiaPhase.DAY,
                day_number=game.day_number,
            )
            voted_ids = {
                v["actor_id"] for v in votes if v["actor_id"] in {p.discord_id for p in alive}
            }
            out["voted_count"] = len(voted_ids)
            out["alive_voters"] = len(alive)
        return out

    def get_player_role(self, guild_id: int | None, discord_id: int) -> dict | None:
        game = self.repo.get_active_game(guild_id)
        if game is None:
            return None
        player = self.repo.get_player(game.game_id, discord_id)
        if player is None:
            return None

        out = {
            "game_id": game.game_id,
            "role": player.role.value,
            "is_godfather": player.is_godfather,
            "hero_name": player.hero_name,
            "is_alive": player.is_alive,
            "phase": game.phase.value,
        }

        if player.role == MafiaRole.MAFIA:
            allies = [
                {
                    "discord_id": p.discord_id,
                    "is_godfather": p.is_godfather,
                    "hero_name": p.hero_name,
                }
                for p in self.repo.get_players(game.game_id)
                if p.role == MafiaRole.MAFIA and p.discord_id != discord_id
            ]
            out["allies"] = allies

        if player.role == MafiaRole.DETECTIVE:
            invests = [
                a for a in self.repo.get_actions(
                    game.game_id, MafiaActionType.INVESTIGATE
                )
                if a["actor_id"] == discord_id
            ]
            out["investigations"] = [
                {"target_id": a["target_id"], "result": a["result"]}
                for a in invests
            ]

        return out

    def get_history(
        self, guild_id: int | None, discord_id: int, limit: int = 20
    ) -> list[dict]:
        return self.repo.get_player_history(discord_id, guild_id, limit)

    def get_leaderboard(
        self, guild_id: int | None, limit: int = 20
    ) -> list[dict]:
        return self.repo.get_leaderboard(guild_id, limit)

    def get_titles(self, guild_id: int | None, discord_id: int) -> list[str]:
        stats = self.repo.compute_player_stats(guild_id, discord_id)
        return [name for name, predicate in TITLES.items() if predicate(stats)]

    # ────────────────────────────────────────────────────────────────────
    # Opt-out / activity gating
    # ────────────────────────────────────────────────────────────────────

    def set_optout(self, guild_id: int | None, discord_id: int, opted_out: bool) -> None:
        self.repo.set_optout(guild_id, discord_id, opted_out)

    def is_active_for_auto_roster(self, guild_id: int | None, discord_id: int) -> bool:
        """Excluded if last AUTO_SKIP_THRESHOLD games all show acted=0."""
        recent = self.repo.get_recent_player_participation(
            discord_id, guild_id, limit=AUTO_SKIP_THRESHOLD
        )
        if len(recent) < AUTO_SKIP_THRESHOLD:
            return True
        return any(recent)  # at least one of the last N showed activity

    # ────────────────────────────────────────────────────────────────────
    # Reminders (called by cog loop)
    # ────────────────────────────────────────────────────────────────────

    def players_needing_night_action(self, game: MafiaGame) -> list[int]:
        """Alive role-bearing players who haven't submitted this night's action."""
        players = self.repo.get_players(game.game_id)
        actions = self.repo.get_actions(
            game.game_id, phase=MafiaPhase.NIGHT, day_number=game.day_number
        )
        actor_action: dict[int, set[str]] = {}
        for a in actions:
            actor_action.setdefault(a["actor_id"], set()).add(a["action_type"])

        missing: list[int] = []
        for p in players:
            if not p.is_alive:
                continue
            expected = _allowed_action_for_role(p.role)
            if expected is None:
                continue
            if expected.value not in actor_action.get(p.discord_id, set()):
                missing.append(p.discord_id)
        return missing

    def players_needing_day_vote(self, game: MafiaGame) -> list[int]:
        players = self.repo.get_players(game.game_id)
        votes = self.repo.get_actions(
            game.game_id, MafiaActionType.VOTE, MafiaPhase.DAY, day_number=game.day_number
        )
        voted = {v["actor_id"] for v in votes}
        return [p.discord_id for p in players if p.is_alive and p.discord_id not in voted]

    # ────────────────────────────────────────────────────────────────────
    # Internals
    # ────────────────────────────────────────────────────────────────────

    def _collect_eligible_players(self, guild_id: int | None) -> list[int]:
        since = int(time.time()) - ELIGIBILITY_WINDOW_S
        candidates = self.repo.get_eligible_player_ids(
            guild_id, since, entry_fee=ENTRY_FEE, max_debt=self.max_debt
        )
        return [pid for pid in candidates if self.is_active_for_auto_roster(guild_id, pid)]

    def _roll_twist(self) -> MafiaTwist | None:
        if self._rng.random() < TWIST_PROBABILITY:
            return self._rng.choice(list(MafiaTwist))
        return None

    def _assign_roles(
        self, game_id: int, guild_id: int | None, player_ids: list[int]
    ) -> list[MafiaPlayer]:
        roster_size = len(player_ids)
        counts = ROLE_TABLE[roster_size]
        order = list(player_ids)
        self._rng.shuffle(order)
        assignments: dict[int, MafiaRole] = {}
        idx = 0
        for role_key, role_enum in [
            ("mafia", MafiaRole.MAFIA),
            ("doctor", MafiaRole.DOCTOR),
            ("detective", MafiaRole.DETECTIVE),
            ("vigilante", MafiaRole.VIGILANTE),
        ]:
            for _ in range(counts[role_key]):
                assignments[order[idx]] = role_enum
                idx += 1
        # Remaining → townie.
        for pid in order[idx:]:
            assignments[pid] = MafiaRole.TOWNIE

        # Optional jester swap: replaces one townie if at least 2 townies remain.
        townies = [pid for pid, r in assignments.items() if r == MafiaRole.TOWNIE]
        if len(townies) >= 2 and self._rng.random() < JESTER_PROBABILITY:
            swap = self._rng.choice(townies)
            assignments[swap] = MafiaRole.JESTER

        # Optional bookie swap (neutral wildcard): replaces one remaining townie,
        # guarding ≥2 townies left so town isn't gutted. Independent of jester.
        townies = [pid for pid, r in assignments.items() if r == MafiaRole.TOWNIE]
        if len(townies) >= 2 and self._rng.random() < BOOKIE_PROBABILITY:
            swap = self._rng.choice(townies)
            assignments[swap] = MafiaRole.BOOKIE

        # Godfather: random among mafia.
        mafia_ids = [pid for pid, r in assignments.items() if r == MafiaRole.MAFIA]
        godfather = self._rng.choice(mafia_ids) if mafia_ids else None

        # Hero flavor.
        heroes = self.hero_provider.sample_unique(roster_size)

        gid = self.repo.normalize_guild_id(guild_id)
        out: list[MafiaPlayer] = []
        for pid, hero in zip(player_ids, heroes, strict=False):
            out.append(
                MafiaPlayer(
                    game_id=game_id,
                    discord_id=pid,
                    guild_id=gid,
                    role=assignments[pid],
                    is_godfather=(pid == godfather),
                    hero_name=hero,
                )
            )
        return out

    def _tally_mafia_kill(
        self,
        kill_actions: list[dict],
        alive_ids: set[int],
        mafia_ids: set[int],
        twist: MafiaTwist | None,
    ) -> list[int]:
        """Return ordered list of mafia kill targets (1 normally, 2 under BLOOD_MOON)."""
        valid = [
            a for a in kill_actions
            if a["target_id"] in alive_ids and a["target_id"] not in mafia_ids
        ]
        n_targets = 2 if twist == MafiaTwist.BLOOD_MOON else 1

        if not valid:
            # No submissions: random kill(s) among alive non-mafia.
            pool = [pid for pid in alive_ids if pid not in mafia_ids]
            if not pool:
                return []
            count = min(n_targets, len(pool))
            return self._rng.sample(pool, count)

        counts = Counter(a["target_id"] for a in valid)
        # Plurality with random tie-break.
        max_votes = max(counts.values())
        leaders = [tid for tid, c in counts.items() if c == max_votes]
        first = self._rng.choice(leaders)
        targets = [first]

        if n_targets > 1:
            # Pick a second distinct target: next plurality, else random alive non-mafia.
            remaining = {tid: c for tid, c in counts.items() if tid != first}
            if remaining:
                m2 = max(remaining.values())
                runners = [tid for tid, c in remaining.items() if c == m2]
                targets.append(self._rng.choice(runners))
            else:
                pool = [
                    pid for pid in alive_ids
                    if pid not in mafia_ids and pid != first
                ]
                if pool:
                    targets.append(self._rng.choice(pool))
        return targets

    def _tally_lynch(self, votes: list[dict]) -> int | None:
        if not votes:
            return None
        counts = Counter(v["target_id"] for v in votes)
        max_votes = max(counts.values())
        leaders = [tid for tid, c in counts.items() if c == max_votes]
        if len(leaders) > 1:
            return None  # tie → no lynch
        return leaders[0]


    def _record_detective_results(
        self, game: MafiaGame, by_id: dict[int, MafiaPlayer]
    ) -> None:
        """Detective results are persisted at submission time, but if a detective
        somehow has an INVESTIGATE row without a stored `result` (older write),
        backfill it here. No-op for normal flows.
        """
        invests = self.repo.get_actions(
            game.game_id, MafiaActionType.INVESTIGATE, MafiaPhase.NIGHT,
            day_number=game.day_number,
        )
        for a in invests:
            if a.get("result"):
                continue
            target = by_id.get(a["target_id"])
            if target is None:
                continue
            verdict = "Town"
            if not target.is_godfather and target.role == MafiaRole.MAFIA:
                verdict = "Mafia"
            self.repo.record_action(
                game_id=game.game_id,
                guild_id=game.guild_id,
                actor_id=a["actor_id"],
                target_id=a["target_id"],
                action_type=MafiaActionType.INVESTIGATE,
                phase=MafiaPhase.NIGHT,
                result=verdict,
                day_number=game.day_number,
            )

    def _bookie_hits_over_week(
        self,
        game: MafiaGame,
        all_players: list[MafiaPlayer],
        *,
        extra_hit: bool = False,
    ) -> int | None:
        """The Bookie cashes if they're alive at the end and called ≥1 lynch.

        Past correct calls are flagged durably (WAGER row result='HIT') as each
        day resolves. ``extra_hit`` counts the day being resolved RIGHT NOW: its
        durable HIT is written only after the resolution is claimed (see
        resolve_day / _commit_post_claim), so it is not yet on record here.
        """
        bookie = next(
            (p for p in all_players if p.role == MafiaRole.BOOKIE), None
        )
        if bookie is None or not bookie.is_alive:
            return None
        wagers = self.repo.get_actions(
            game.game_id, MafiaActionType.WAGER, MafiaPhase.NIGHT
        )
        hits = sum(
            1
            for w in wagers
            if w["actor_id"] == bookie.discord_id and w.get("result") == "HIT"
        )
        if extra_hit:
            hits += 1
        return bookie.discord_id if hits >= 1 else None

    def _bookie_hit_today(
        self, game: MafiaGame, dn: int, lynched_id: int | None
    ) -> bool:
        """Whether this day's Bookie wager called the lynch correctly. Computed
        in memory so it can feed this day's skim before finalize; the matching
        durable HIT is written only after the resolution is claimed."""
        if lynched_id is None:
            return False
        wagers = self.repo.get_actions(
            game.game_id, MafiaActionType.WAGER, MafiaPhase.NIGHT, day_number=dn
        )
        return any(w["target_id"] == lynched_id for w in wagers)

    def _winners_for(
        self, all_players: list[MafiaPlayer], winner: MafiaWinner
    ) -> list[int]:
        if winner == MafiaWinner.TOWN:
            return [p.discord_id for p in all_players if p.role in TOWN_ROLES]
        if winner == MafiaWinner.MAFIA:
            return [p.discord_id for p in all_players if p.role in MAFIA_ROLES]
        if winner == MafiaWinner.JESTER:
            return [p.discord_id for p in all_players if p.role == MafiaRole.JESTER]
        return []

    def _compute_mvp(
        self,
        game: MafiaGame,
        all_players: list[MafiaPlayer],
        winner: MafiaWinner,
        lynched_id: int | None,
        valid_votes: list[dict],
    ) -> int | None:
        if winner == MafiaWinner.NONE:
            return None

        if winner == MafiaWinner.JESTER:
            for p in all_players:
                if p.role == MafiaRole.JESTER:
                    return p.discord_id
            return None

        if winner == MafiaWinner.TOWN and lynched_id is not None:
            # Random non-mafia voter who voted for the lynched player.
            voters_for = [
                v["actor_id"] for v in valid_votes if v["target_id"] == lynched_id
            ]
            non_mafia_voters = [
                vid for vid in voters_for
                if any(p.discord_id == vid and p.role != MafiaRole.MAFIA for p in all_players)
            ]
            if non_mafia_voters:
                return self._rng.choice(non_mafia_voters)

        if winner == MafiaWinner.MAFIA:
            # Random mafia whose KILL submission matched an actual victim.
            kill_actions = self.repo.get_actions(
                game.game_id, MafiaActionType.KILL, MafiaPhase.NIGHT
            )
            actual_victims = {
                p.discord_id
                for p in all_players
                if not p.is_alive and p.eliminated_phase == MafiaPhase.NIGHT
            }
            mafia_ids = {p.discord_id for p in all_players if p.role == MafiaRole.MAFIA}
            scoring_killers = [
                a["actor_id"] for a in kill_actions
                if a["actor_id"] in mafia_ids and a["target_id"] in actual_victims
            ]
            if scoring_killers:
                return self._rng.choice(scoring_killers)
            if mafia_ids:
                return self._rng.choice(list(mafia_ids))

        # Generic fallback: random winner.
        winning_ids = self._winners_for(all_players, winner)
        return self._rng.choice(winning_ids) if winning_ids else None

    @staticmethod
    def _vote_breakdown(votes: list[dict]) -> dict[int, int]:
        return dict(Counter(v["target_id"] for v in votes))

    @staticmethod
    def _vote_detail(votes: list[dict]) -> list[dict]:
        """Per-voter (who → whom) for the dusk reveal."""
        return [
            {"actor_id": v["actor_id"], "target_id": v["target_id"]} for v in votes
        ]
