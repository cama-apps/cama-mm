"""RecordingMixin mixin for :class:`MatchService`.

Match recording: the atomic ``record_match`` finalization plus its post-match
money/bonus/loan/easter-egg helpers, and the test-seeding ``record_match_raw``.

Mixin split out of the former monolithic ``match_service`` module; it carries
no state of its own and is composed into ``MatchService``.
"""

import time
from datetime import UTC, datetime

from config import CALIBRATION_RD_THRESHOLD
from domain.models.pending_match_state import PendingMatchState
from openskill_rating_system import CamaOpenSkillSystem
from services.match._common import coalesce_os_baseline, logger
from utils.guild import normalize_guild_id


class RecordingMixin:
    """RecordingMixin — see module docstring.

    Composed into :class:`~services.match_service.MatchService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    def _settle_match_bets_and_bonuses(
        self,
        match_id: int,
        winning_team: str,
        winning_ids: list[int],
        losing_ids: list[int],
        excluded_player_ids: list[int],
        last_shuffle: PendingMatchState,
        guild_id: int | None,
    ) -> dict:
        """Pay participation, win, bomb-pot, and exclusion bonuses, then settle
        the pot. Returns the bet distributions (empty dict if no betting service)."""
        distributions: dict = {"winners": [], "losers": []}
        if not self.betting_service:
            return distributions

        is_bomb_pot = last_shuffle.is_bomb_pot
        # Track actual net JC paid per player so we can persist it on
        # match_participants. Without this, balance-history reconstruction
        # would silently drift if reward constants or penalty rules change.
        bonus_net: dict[int, int] = {}

        def _accumulate(awards: dict[int, dict[str, int]]) -> None:
            for pid, amounts in awards.items():
                bonus_net[pid] = bonus_net.get(pid, 0) + int(amounts.get("net", 0))

        _accumulate(
            self.betting_service.award_participation(losing_ids, guild_id, is_bomb_pot=is_bomb_pot)
        )
        if is_bomb_pot:
            # Winners also get the bomb-pot bonus (+1 JC) on top of their win bonus.
            _accumulate(
                self.betting_service.award_participation(
                    winning_ids, guild_id, is_bomb_pot=True, bomb_pot_bonus_only=True
                )
            )

        distributions = self.betting_service.settle_bets(
            match_id, guild_id, winning_team, pending_state=last_shuffle
        )
        _accumulate(self.betting_service.award_win_bonus(winning_ids, guild_id))
        if excluded_player_ids:
            _accumulate(
                self.betting_service.award_exclusion_bonus(excluded_player_ids, guild_id)
            )
        excluded_conditional_ids = last_shuffle.excluded_conditional_player_ids
        if excluded_conditional_ids:
            _accumulate(
                self.betting_service.award_exclusion_bonus_half(excluded_conditional_ids, guild_id)
            )

        # Daily-play streak bonus. Only fires for actual players (winning_ids +
        # losing_ids, never bench/excluded), and reuses the same 4 AM PST
        # rollover that /dig uses so the two systems can't drift on day math.
        streaks = self._award_dota_streak_bonuses(
            winning_ids + losing_ids, guild_id, _accumulate
        )
        distributions["streaks"] = streaks

        if bonus_net and hasattr(self.match_repo, "update_participant_bonus_jc"):
            self.match_repo.update_participant_bonus_jc(match_id, guild_id, bonus_net)

        return distributions

    def _award_dota_streak_bonuses(
        self,
        actual_player_ids: list[int],
        guild_id: int | None,
        accumulate,
    ) -> dict[int, dict[str, int]]:
        """Advance each player's Dota streak and credit the matching tier bonus.

        Returns ``{discord_id: {"days": N, "bonus": JC}}``. Players without a
        streak bonus (below the 3-day floor) are still included with bonus=0
        so the embed can decide whether to render the line.
        """
        from services.dig_constants import STREAKS
        from utils.game_date import get_game_date, streak_bonus_for, yesterday_of

        if not actual_player_ids:
            return {}
        today = get_game_date()
        yesterday = yesterday_of(today)

        result: dict[int, dict[str, int]] = {}
        for pid in actual_player_ids:
            new_streak = self.player_repo.advance_dota_streak(
                pid, guild_id, today, yesterday
            )
            bonus = streak_bonus_for(new_streak, STREAKS)
            result[pid] = {"days": new_streak, "bonus": bonus}
            if bonus > 0:
                self.player_repo.add_balance(
                    pid,
                    guild_id,
                    bonus,
                    source="match_streak",
                    related_type="dota_streak",
                    reason="Dota streak match bonus",
                    metadata={"streak_days": new_streak, "bonus": bonus},
                )
                net_bonus = bonus
                if self.betting_service:
                    skimmed = self.betting_service._apply_blood_pact_skim(
                        pid, guild_id, bonus
                    )
                    net_bonus = bonus - skimmed
                accumulate({pid: {"net": net_bonus}})
        return result

    def _repay_outstanding_loans(
        self, participant_ids: list[int], guild_id: int | None
    ) -> list[dict]:
        """Attempt loan repayment for every participant with an outstanding loan.
        Returns one dict per successful repayment (order follows input)."""
        if not self.loan_service:
            return []
        repayments: list[dict] = []
        for player_id in participant_ids:
            state = self.loan_service.get_state(player_id, guild_id)
            if not state.has_outstanding_loan:
                continue
            result = self.loan_service.execute_repayment(player_id, guild_id)
            if not result.success:
                continue
            r = result.value
            repayments.append({
                "player_id": player_id,
                "success": True,
                "principal": r.principal,
                "fee": r.fee,
                "total_repaid": r.total_repaid,
                "balance_before": r.balance_before,
                "new_balance": r.new_balance,
                "nonprofit_total": r.nonprofit_total,
            })
        return repayments

    def _collect_match_easter_eggs(
        self,
        radiant_team_ids: list[int],
        dire_team_ids: list[int],
        winning_ids: list[int],
        expected_ids: set[int],
        streak_data: dict[int, tuple[int, float]],
        guild_id: int | None,
    ) -> dict:
        """Collect rivalry detections, games milestones, and personal-best
        streak records into a single dict for post-record neon hooks."""
        easter_egg_data: dict = {
            "games_milestones": [],
            "win_streak_records": [],
            "rivalries_detected": [],
        }

        if self.pairings_repo:
            try:
                all_players = radiant_team_ids + dire_team_ids
                for i, p1 in enumerate(all_players):
                    for p2 in all_players[i + 1:]:
                        pairing = self.pairings_repo.get_pairing(p1, p2, guild_id)
                        if not pairing or pairing.get("games_together", 0) < 10:
                            continue
                        wins = pairing.get("p1_wins", 0)
                        losses = pairing.get("p1_losses", 0)
                        total = wins + losses
                        if total < 10:
                            continue
                        winrate = (wins / total) * 100
                        if winrate >= 70 or winrate <= 30:
                            easter_egg_data["rivalries_detected"].append({
                                "player1_id": p1,
                                "player2_id": p2,
                                "games_together": total,
                                "winrate_vs": winrate,
                            })
            except Exception as e:
                logger.debug(f"Rivalry detection error: {e}")

        milestone_values = {10, 50, 100, 200, 500}
        milestone_players = self.player_repo.get_by_ids(list(expected_ids), guild_id)
        for player in milestone_players:
            total_games = player.wins + player.losses
            if total_games in milestone_values:
                easter_egg_data["games_milestones"].append({
                    "discord_id": player.discord_id,
                    "total_games": total_games,
                })

        for pid in winning_ids:
            slen, _ = streak_data.get(pid, (1, 1.0))
            if slen >= 5 and hasattr(self.player_repo, "get_personal_best_win_streak"):
                prev_best = self.player_repo.get_personal_best_win_streak(pid, guild_id)
                if slen > prev_best:
                    self.player_repo.update_personal_best_win_streak(pid, guild_id, slen)
                    easter_egg_data["win_streak_records"].append({
                        "discord_id": pid,
                        "current_streak": slen,
                        "previous_best": prev_best,
                    })

        return easter_egg_data

    def record_match(
        self,
        winning_team: str,
        guild_id: int | None = None,
        dotabuff_match_id: str | None = None,
        pending_match_id: int | None = None,
    ) -> dict:
        """
        Record a match result and update ratings.

        winning_team: 'radiant' or 'dire'
        pending_match_id: Optional specific match ID for concurrent match support.
                         If None and only one pending match exists, uses that one.

        Thread-safe: prevents concurrent finalization for the same match.
        """
        normalized_gid = normalize_guild_id(guild_id)

        # Acquire lock BEFORE reading shuffle state to prevent TOCTOU race:
        # Without this, two threads could both read the same shuffle state
        # before either acquires the lock, causing double-recording.
        with self._recording_lock:
            last_shuffle = self.get_last_shuffle(guild_id, pending_match_id)
            if not last_shuffle:
                raise ValueError("No recent shuffle found.")
            pending_match_id = last_shuffle.pending_match_id
            lock_key = (normalized_gid, pending_match_id)
            if lock_key in self._recording_in_progress:
                match_note = f" (Match #{pending_match_id})" if pending_match_id else ""
                raise ValueError(f"Match recording already in progress{match_note}.")
            self._recording_in_progress.add(lock_key)

        try:

            if winning_team not in ("radiant", "dire"):
                raise ValueError("winning_team must be 'radiant' or 'dire'.")

            radiant_team_ids = last_shuffle.radiant_team_ids
            dire_team_ids = last_shuffle.dire_team_ids
            excluded_player_ids = last_shuffle.excluded_player_ids

            all_match_ids = set(radiant_team_ids + dire_team_ids)
            if set(excluded_player_ids).intersection(all_match_ids):
                raise ValueError("Excluded players detected in match teams.")

            # Map winners/losers for DB
            winning_ids = radiant_team_ids if winning_team == "radiant" else dire_team_ids
            losing_ids = dire_team_ids if winning_team == "radiant" else radiant_team_ids

            # Determine lobby type from pending state (draft sets is_draft=True)
            lobby_type = "draft" if last_shuffle.is_draft else "shuffle"
            balancing_rating_system = last_shuffle.balancing_rating_system
            betting_mode = last_shuffle.betting_mode

            # ---- PURE COMPUTATION (reads only; safe to run before the atomic block) ----
            radiant_glicko = [self._load_glicko_player(pid, guild_id) for pid in radiant_team_ids]
            dire_glicko = [self._load_glicko_player(pid, guild_id) for pid in dire_team_ids]

            all_player_ids = radiant_team_ids + dire_team_ids
            streak_multipliers: dict[int, float] = {}
            streak_data: dict[int, tuple[int, float]] = {}
            for pid in all_player_ids:
                won = (pid in radiant_team_ids and winning_team == "radiant") or \
                      (pid in dire_team_ids and winning_team == "dire")
                recent_outcomes = self.match_repo.get_player_recent_outcomes(pid, normalized_gid, limit=20)
                streak_length, multiplier = self.rating_system.calculate_streak_multiplier(
                    recent_outcomes, won=won
                )
                streak_multipliers[pid] = multiplier
                streak_data[pid] = (streak_length, multiplier)

            pre_match: dict[int, dict] = {}
            for player, pid in radiant_glicko:
                pre_match[pid] = {
                    "rating_before": player.rating,
                    "rd_before": player.rd,
                    "volatility_before": player.vol,
                    "team_number": 1,
                    "won": winning_team == "radiant",
                    "streak_length": streak_data.get(pid, (1, 1.0))[0],
                    "streak_multiplier": streak_data.get(pid, (1, 1.0))[1],
                }
            for player, pid in dire_glicko:
                pre_match[pid] = {
                    "rating_before": player.rating,
                    "rd_before": player.rd,
                    "volatility_before": player.vol,
                    "team_number": 2,
                    "won": winning_team == "dire",
                    "streak_length": streak_data.get(pid, (1, 1.0))[0],
                    "streak_multiplier": streak_data.get(pid, (1, 1.0))[1],
                }

            radiant_rating, radiant_rd, _ = self.rating_system.aggregate_team_stats(
                [p for p, _ in radiant_glicko]
            )
            dire_rating, dire_rd, _ = self.rating_system.aggregate_team_stats(
                [p for p, _ in dire_glicko]
            )
            expected_radiant_win_prob = self.rating_system.expected_outcome(
                radiant_rating, radiant_rd, dire_rating, dire_rd
            )
            expected_team_win_prob = {
                1: expected_radiant_win_prob,
                2: 1.0 - expected_radiant_win_prob,
            }

            if winning_team == "radiant":
                team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                    radiant_glicko, dire_glicko, 1, streak_multipliers=streak_multipliers
                )
            else:
                team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                    dire_glicko, radiant_glicko, 1, streak_multipliers=streak_multipliers
                )

            expected_ids = set(all_player_ids)
            glicko_updates = [
                (pid, rating, rd, vol)
                for rating, rd, vol, pid in team1_updated + team2_updated
                if pid in expected_ids
            ]

            # Phase 1 OpenSkill update (equal weights). Phase 2 happens post-enrichment.
            os_ratings = self.player_repo.get_openskill_ratings_bulk(all_player_ids, guild_id)
            radiant_os_data = [
                (pid, *os_ratings.get(pid, (None, None))) for pid in radiant_team_ids
            ]
            dire_os_data = [
                (pid, *os_ratings.get(pid, (None, None))) for pid in dire_team_ids
            ]
            os_results = self.openskill_system.update_ratings_equal_weight(
                radiant_os_data, dire_os_data,
                winning_team=1 if winning_team == "radiant" else 2,
            )
            os_updates = [(pid, mu, sigma) for pid, (mu, sigma) in os_results.items()]

            DEFAULT_MU = CamaOpenSkillSystem.DEFAULT_MU
            DEFAULT_SIGMA = CamaOpenSkillSystem.DEFAULT_SIGMA
            for pid, (new_mu, new_sigma) in os_results.items():
                old_mu, old_sigma = os_ratings.get(pid, (None, None))
                if pid in pre_match:
                    os_mu_before, os_sigma_before = coalesce_os_baseline(
                        old_mu, old_sigma, DEFAULT_MU, DEFAULT_SIGMA
                    )
                    pre_match[pid]["os_mu_before"] = os_mu_before
                    pre_match[pid]["os_mu_after"] = new_mu
                    pre_match[pid]["os_sigma_before"] = os_sigma_before
                    pre_match[pid]["os_sigma_after"] = new_sigma

            rating_history_rows: list[dict] = []
            for pid, rating, rd, vol in glicko_updates:
                pre = pre_match.get(pid)
                if not pre:
                    continue
                rating_history_rows.append({
                    "discord_id": pid,
                    "rating": rating,
                    "rating_before": pre["rating_before"],
                    "rd_before": pre["rd_before"],
                    "rd_after": rd,
                    "volatility_before": pre["volatility_before"],
                    "volatility_after": vol,
                    "expected_team_win_prob": expected_team_win_prob.get(pre["team_number"]),
                    "team_number": pre["team_number"],
                    "won": pre["won"],
                    "os_mu_before": pre.get("os_mu_before"),
                    "os_mu_after": pre.get("os_mu_after"),
                    "os_sigma_before": pre.get("os_sigma_before"),
                    "os_sigma_after": pre.get("os_sigma_after"),
                    "streak_length": pre.get("streak_length"),
                    "streak_multiplier": pre.get("streak_multiplier"),
                })

            # First-calibration: precompute which players need first_calibrated_at
            # set, so the atomic block can apply the conditional UPDATE without
            # a read-per-player detour inside the transaction.
            now_unix = int(time.time())
            first_calibration_ids: list[int] = []
            if hasattr(self.player_repo, "get_first_calibrated_at"):
                for pid, _rating, rd, _vol in glicko_updates:
                    if (
                        rd <= CALIBRATION_RD_THRESHOLD
                        and self.player_repo.get_first_calibrated_at(pid, guild_id) is None
                    ):
                        first_calibration_ids.append(pid)

            # Consumable charges: only consumed for shuffle mode (not draft).
            effective_avoid_ids: list[int] = []
            effective_deal_ids: list[int] = []
            if not last_shuffle.is_draft:
                if self.soft_avoid_repo:
                    effective_avoid_ids = list(last_shuffle.effective_avoid_ids)
                if self.package_deal_repo:
                    effective_deal_ids = list(last_shuffle.effective_deal_ids)

            # ---- ATOMIC WRITE: match + participants + wins/losses + glicko +
            # OpenSkill + last_match_date + first_calibrated_at + match_prediction
            # + rating_history + pairings + consumable decrements, all in one
            # BEGIN IMMEDIATE. A crash here rolls the whole match back, so the
            # invariant "every committed match has matching rating_history and
            # pairings" holds.
            now_iso = datetime.now(UTC).isoformat()
            match_prediction = {
                "radiant_rating": radiant_rating,
                "dire_rating": dire_rating,
                "radiant_rd": radiant_rd,
                "dire_rd": dire_rd,
                "expected_radiant_win_prob": expected_radiant_win_prob,
            }

            match_id = self.match_repo.record_match_core_atomic(
                team1_ids=radiant_team_ids,
                team2_ids=dire_team_ids,
                winning_team=1 if winning_team == "radiant" else 2,
                guild_id=guild_id,
                dotabuff_match_id=dotabuff_match_id,
                lobby_type=lobby_type,
                balancing_rating_system=balancing_rating_system,
                betting_mode=betting_mode,
                winning_ids=winning_ids,
                losing_ids=losing_ids,
                glicko_updates=glicko_updates,
                openskill_updates=os_updates,
                rating_history_rows=rating_history_rows,
                match_prediction=match_prediction,
                last_match_date_iso=now_iso,
                first_calibration_ids=first_calibration_ids,
                first_calibration_unix=now_unix,
                effective_avoid_ids=effective_avoid_ids,
                effective_deal_ids=effective_deal_ids,
                pending_match_id=pending_match_id,
            )
            updated_count = len(glicko_updates)

            # ---- POST-MATCH: money side (bets + loans) runs in its own
            # transactions. Pending bets stay pending and loans stay outstanding
            # if these fail, so retry/recovery happens via the usual paths.
            distributions = self._settle_match_bets_and_bonuses(
                match_id, winning_team, winning_ids, losing_ids,
                excluded_player_ids, last_shuffle, guild_id,
            )
            loan_repayments = self._repay_outstanding_loans(
                winning_ids + losing_ids, guild_id
            )

            # Find the most notable streak (longest, >=5 games) for neon hooks
            notable_streak = None
            for pid, (slen, _smult) in streak_data.items():
                if slen >= 5 and (notable_streak is None or slen > notable_streak["streak"]):
                    won = (
                        (pid in radiant_team_ids and winning_team == "radiant")
                        or (pid in dire_team_ids and winning_team == "dire")
                    )
                    notable_streak = {
                        "discord_id": pid,
                        "streak": slen,
                        "is_win": won,
                    }

            easter_egg_data = self._collect_match_easter_eggs(
                radiant_team_ids, dire_team_ids, winning_ids, expected_ids,
                streak_data, guild_id,
            )

            # Clear state after successful record (only this specific match)
            self.clear_last_shuffle(guild_id, pending_match_id)

            return {
                "match_id": match_id,
                "winning_team": winning_team,
                "updated_count": updated_count,
                "winning_player_ids": winning_ids,
                "losing_player_ids": losing_ids,
                "excluded_player_ids": excluded_player_ids,
                "excluded_conditional_player_ids": last_shuffle.excluded_conditional_player_ids,
                "bet_distributions": distributions,
                "loan_repayments": loan_repayments,
                "notable_streak": notable_streak,
                "easter_egg_data": easter_egg_data,
            }
        finally:
            with self._recording_lock:
                self._recording_in_progress.discard(lock_key)

    def record_match_raw(
        self,
        team1_ids: list[int],
        team2_ids: list[int],
        winning_team: int,
        guild_id: int | None = None,
        lobby_type: str = "shuffle",
    ) -> int:
        """
        Record a match directly without shuffle state or rating updates.

        Used for test data seeding. For normal match recording with ratings,
        use record_match() instead.

        Args:
            team1_ids: Discord IDs of Radiant players
            team2_ids: Discord IDs of Dire players
            winning_team: 1 (Radiant won) or 2 (Dire won)
            guild_id: Guild ID for multi-server isolation
            lobby_type: 'shuffle' or 'draft'

        Returns:
            Match ID
        """
        return self.match_repo.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=winning_team,
            guild_id=guild_id or 0,
            lobby_type=lobby_type,
        )
