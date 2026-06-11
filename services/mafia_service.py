"""Daily Mafia subgame service.

Encapsulates roster collection, role assignment, phase resolution, payouts,
and read-side queries for the /mafia command surface.
"""

import logging
import random
import sqlite3
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

# Phase clock (seconds since started_at).
NIGHT_DURATION_S = 6 * 3600
DAY_DURATION_S = 13 * 3600
PHASE_REMINDER_LEAD_S = 3600

# Probabilities.
JESTER_PROBABILITY = 0.20
TWIST_PROBABILITY = 0.30

# Eligibility window for /gamba and /dig activity.
ELIGIBILITY_WINDOW_S = 24 * 3600

# Auto-skip: if a player's last N games all show acted=0, exclude from auto-roster.
AUTO_SKIP_THRESHOLD = 3

# Economy. Each rostered player is charged ENTRY_FEE at game start; the fees
# pool into a single pot distributed among the winning faction at day
# resolution. Because roles are assigned uniformly at random, the per-player
# expectation of the pot share equals exactly ENTRY_FEE regardless of the
# (unknown) faction win-rate distribution — so EV per game = 0 by construction.
# MVP_BONUS is now drawn from the pot rather than minted.
ENTRY_FEE = 30
MVP_BONUS = 20

TITLES: dict[str, callable] = {
    "Don of Dire":  lambda s: s["mafia_wins"] >= 10,
    "Inquisitor":   lambda s: s["correct_reads"] >= 5,
    "Reaper":       lambda s: s["mafia_kills"] >= 5,
    "Lifesaver":    lambda s: s["doctor_saves"] >= 5,
    "Wildcard":     lambda s: s["jester_wins"] >= 1,
}


def _pot_for_roster(roster_size: int) -> int:
    return roster_size * ENTRY_FEE


def _allowed_action_for_role(role: MafiaRole) -> MafiaActionType | None:
    return {
        MafiaRole.MAFIA: MafiaActionType.KILL,
        MafiaRole.DOCTOR: MafiaActionType.SAVE,
        MafiaRole.DETECTIVE: MafiaActionType.INVESTIGATE,
        MafiaRole.VIGILANTE: MafiaActionType.VIG_KILL,
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

    def start_daily_game(self, guild_id: int | None) -> MafiaGame | None:
        """Idempotent. Creates today's game if not already started.

        Returns the new game on creation, the existing game if today's already
        exists, or None if the eligible roster is too small.
        """
        game_date = self.dig_service._get_game_date()
        existing = self.repo.get_game_for_date(guild_id, game_date)
        if existing is not None:
            return existing

        for attempt in range(2):
            eligible = self._collect_eligible_players(guild_id)
            if len(eligible) < MIN_ROSTER:
                logger.info(
                    "Mafia start skipped for guild %s: only %d eligible players",
                    guild_id,
                    len(eligible),
                )
                return None

            if len(eligible) > MAX_ROSTER:
                eligible = self._rng.sample(eligible, MAX_ROSTER)

            roster_size = len(eligible)
            twist = self._roll_twist()
            started_at = int(time.time())
            players = self._assign_roles(0, guild_id, eligible)

            try:
                game_id = self.repo.create_game_with_players_and_entry_fees(
                    guild_id=guild_id,
                    game_date=game_date,
                    phase=MafiaPhase.NIGHT,
                    started_at=started_at,
                    roster_size=roster_size,
                    twist_event=twist,
                    entry_fee=ENTRY_FEE,
                    players=players,
                    max_debt=self.max_debt,
                )
            except sqlite3.IntegrityError:
                existing = self.repo.get_game_for_date(guild_id, game_date)
                if existing is not None:
                    return existing
                raise
            except ValueError as exc:
                if str(exc) == "entry_fee_debit_failed" and attempt == 0:
                    continue
                logger.info(
                    "Mafia start skipped for guild %s: entry fee debit failed",
                    guild_id,
                )
                return None

            game = self.repo.get_game_by_id(game_id)
            if game is not None:
                game.players = self.repo.get_players(game_id)
            return game

        return None

    def resolve_night(self, guild_id: int | None) -> dict:
        """Apply night actions, mark the dead, advance to DAY.

        Returns a summary dict for the cog to render publicly.
        """
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase != MafiaPhase.NIGHT:
            return {"resolved": False, "reason": "no_active_night"}

        all_players = self.repo.get_players(game.game_id)
        by_id = {p.discord_id: p for p in all_players}
        alive_ids = {p.discord_id for p in all_players if p.is_alive}
        mafia_ids = {p.discord_id for p in all_players if p.role == MafiaRole.MAFIA}

        kill_actions = self.repo.get_actions(
            game.game_id, MafiaActionType.KILL, MafiaPhase.NIGHT
        )
        save_actions = self.repo.get_actions(
            game.game_id, MafiaActionType.SAVE, MafiaPhase.NIGHT
        )
        vig_actions = self.repo.get_actions(
            game.game_id, MafiaActionType.VIG_KILL, MafiaPhase.NIGHT
        )

        # Determine mafia kill targets.
        mafia_kill_targets = self._tally_mafia_kill(
            kill_actions, alive_ids, mafia_ids, by_id, twist=game.twist_event
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

        # Process detective investigations: persist results.
        if game.twist_event != MafiaTwist.MEMORY_FOG:
            self._record_detective_results(game, by_id)

        applied = self.repo.apply_night_resolution(
            game.game_id, killed_ids, ended_at=int(time.time())
        )
        if not applied:
            return {"resolved": False, "reason": "night_already_resolved"}

        return {
            "resolved": True,
            "game_id": game.game_id,
            "twist": game.twist_event.value if game.twist_event else None,
            "killed": killed,
            "save_blocked": save_target is not None and save_target not in [k["discord_id"] for k in killed],
        }

    def resolve_day(self, guild_id: int | None) -> dict:
        """Tally votes, apply lynch, evaluate win condition, pay out."""
        game = self.repo.get_active_game(guild_id)
        if game is None or game.phase != MafiaPhase.DAY:
            return {"resolved": False, "reason": "no_active_day"}

        all_players = self.repo.get_players(game.game_id)
        by_id = {p.discord_id: p for p in all_players}
        alive = [p for p in all_players if p.is_alive]
        alive_ids = {p.discord_id for p in alive}

        votes = self.repo.get_actions(
            game.game_id, MafiaActionType.VOTE, MafiaPhase.DAY
        )
        # Only count votes from alive players targeting alive players.
        valid_votes = [
            v for v in votes
            if v["actor_id"] in alive_ids and v["target_id"] in alive_ids
        ]

        lynched_id: int | None = None
        if game.twist_event != MafiaTwist.TOWN_HALL:
            lynched_id = self._tally_lynch(valid_votes)

        if lynched_id is not None and lynched_id in by_id:
            by_id[lynched_id].is_alive = False
            by_id[lynched_id].eliminated_phase = MafiaPhase.DAY

        # Evaluate against the local post-lynch state. The DB write happens
        # atomically with payouts below.
        all_players = list(by_id.values())

        # Win condition: jester first (overrides everything).
        winner: MafiaWinner
        if lynched_id is not None and by_id[lynched_id].role == MafiaRole.JESTER:
            winner = MafiaWinner.JESTER
        else:
            alive = [p for p in all_players if p.is_alive]
            alive_mafia = sum(1 for p in alive if p.role == MafiaRole.MAFIA)
            alive_non_mafia = sum(1 for p in alive if p.role != MafiaRole.MAFIA)
            if alive_mafia == 0:
                winner = MafiaWinner.TOWN
            elif alive_mafia >= alive_non_mafia:
                winner = MafiaWinner.MAFIA
            else:
                # One-day cadence: don't loop. Mafia wins by attrition if not pinned.
                winner = MafiaWinner.MAFIA if alive_mafia > 0 else MafiaWinner.TOWN

        entry_fee = game.entry_fee or ENTRY_FEE
        pot_total = game.roster_size * entry_fee
        winning_ids = self._winners_for(all_players, winner)
        mvp_id = self._compute_mvp(
            game, all_players, winner, lynched_id, valid_votes
        )

        # Split the pot among winners. MVP_BONUS + integer-division remainder
        # ride on top of the winning faction's MVP share — within-faction
        # redistribution conserves the faction total, so EV per random role
        # assignment stays exactly zero. The dust must always be allocated to
        # some winner (otherwise zero-sum leaks).
        deltas: dict[int, int] = {}
        if winning_ids:
            mvp_in_winners = mvp_id is not None and mvp_id in winning_ids
            mvp_share = MVP_BONUS if mvp_in_winners else 0
            remainder_pot = pot_total - mvp_share
            base_payout = remainder_pot // len(winning_ids)
            dust = remainder_pot - base_payout * len(winning_ids)
            for wid in winning_ids:
                deltas[wid] = base_payout
            # Bonus + rounding dust go to the MVP if there is one; otherwise
            # the dust falls to the first winner (deterministic, faction-local).
            if mvp_in_winners:
                deltas[mvp_id] += mvp_share + dust
            elif dust:
                deltas[winning_ids[0]] += dust
            payout_per_winner = base_payout
        else:
            payout_per_winner = 0

        finalize = self.repo.finalize_day_resolution(
            game_id=game.game_id,
            winner=winner,
            payout_per_winner=payout_per_winner,
            mvp_id=mvp_id,
            lynched_id=lynched_id,
            payout_deltas=deltas,
            entry_fee=entry_fee,
            bankruptcy_penalty_rate=self.bankruptcy_penalty_rate,
        )
        if not finalize.get("applied"):
            return {
                "resolved": False,
                "reason": finalize.get("reason", "day_already_resolved"),
            }

        return {
            "resolved": True,
            "game_id": game.game_id,
            "winner": winner.value,
            "lynched_id": lynched_id,
            "mvp_id": mvp_id,
            "payout_per_winner": payout_per_winner,
            "pot_total": pot_total,
            "entry_fee": entry_fee,
            "winning_ids": list(winning_ids),
            "bankruptcy_penalties": finalize.get("bankruptcy_penalties", {}),
            "vote_breakdown": self._vote_breakdown(valid_votes),
            "twist": game.twist_event.value if game.twist_event else None,
        }

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

        self.repo.record_action(
            game_id=game.game_id,
            guild_id=guild_id,
            actor_id=voter_id,
            target_id=target_id,
            action_type=MafiaActionType.VOTE,
            phase=MafiaPhase.DAY,
        )
        return {"ok": True}

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

        out = {
            "active": True,
            "game_id": game.game_id,
            "phase": game.phase.value,
            "started_at": game.started_at,
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

        if game.phase == MafiaPhase.NIGHT:
            out["phase_ends_at"] = game.started_at + NIGHT_DURATION_S
        elif game.phase == MafiaPhase.DAY:
            out["phase_ends_at"] = game.started_at + NIGHT_DURATION_S + DAY_DURATION_S
            votes = self.repo.get_actions(
                game.game_id, MafiaActionType.VOTE, MafiaPhase.DAY
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
        """Alive role-bearing players who haven't submitted their night action."""
        players = self.repo.get_players(game.game_id)
        actions = self.repo.get_actions(game.game_id, phase=MafiaPhase.NIGHT)
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
        votes = self.repo.get_actions(game.game_id, MafiaActionType.VOTE, MafiaPhase.DAY)
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
        by_id: dict[int, MafiaPlayer],
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

    def _kill_player(
        self,
        game_id: int,
        discord_id: int,
        phase: MafiaPhase,
        by_id: dict[int, MafiaPlayer],
    ) -> None:
        self.repo.set_player_alive(
            game_id, discord_id, alive=False, eliminated_phase=phase
        )
        if discord_id in by_id:
            by_id[discord_id].is_alive = False
            by_id[discord_id].eliminated_phase = phase

    def _record_detective_results(
        self, game: MafiaGame, by_id: dict[int, MafiaPlayer]
    ) -> None:
        """Detective results are persisted at submission time, but if a detective
        somehow has an INVESTIGATE row without a stored `result` (older write),
        backfill it here. No-op for normal flows.
        """
        invests = self.repo.get_actions(
            game.game_id, MafiaActionType.INVESTIGATE, MafiaPhase.NIGHT
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
            )

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
