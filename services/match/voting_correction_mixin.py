"""VotingCorrectionMixin mixin for :class:`MatchService`.

Result-vote and abort-vote management (thin delegators over
``MatchVotingService``) plus the after-the-fact match-result correction that
reverses and re-applies an already-recorded match.

Mixin split out of the former monolithic ``match_service`` module; it carries
no state of its own and is composed into ``MatchService``.
"""

from typing import Any

from services.match._common import logger


class VotingCorrectionMixin:
    """VotingCorrectionMixin — see module docstring.

    Composed into :class:`~services.match_service.MatchService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    # ==================== Voting Management (delegated to MatchVotingService) ====================

    def has_admin_submission(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if an admin has submitted a result vote (delegates to voting_service)."""
        return self.voting_service.has_admin_submission(guild_id, pending_match_id)

    def has_admin_abort_submission(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if an admin has submitted an abort vote (delegates to voting_service)."""
        return self.voting_service.has_admin_abort_submission(guild_id, pending_match_id)

    def add_record_submission(
        self, guild_id: int | None, user_id: int, result: str, is_admin: bool,
        pending_match_id: int | None = None
    ) -> dict[str, Any]:
        """Add a vote for the match result (delegates to voting_service)."""
        return self.voting_service.add_record_submission(guild_id, user_id, result, is_admin, pending_match_id)

    def get_non_admin_submission_count(self, guild_id: int | None, pending_match_id: int | None = None) -> int:
        """Get count of non-admin result votes (delegates to voting_service)."""
        return self.voting_service.get_non_admin_submission_count(guild_id, pending_match_id)

    def get_abort_submission_count(self, guild_id: int | None, pending_match_id: int | None = None) -> int:
        """Get count of non-admin abort votes (delegates to voting_service)."""
        return self.voting_service.get_abort_submission_count(guild_id, pending_match_id)

    def can_abort_match(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if there are enough votes to abort (delegates to voting_service)."""
        return self.voting_service.can_abort_match(guild_id, pending_match_id)

    def get_pending_match_for_abort_voter(self, guild_id: int | None, user_id: int):
        """Find the pending match whose shuffled lobby can cast an abort vote."""
        return self.voting_service.get_pending_match_for_abort_voter(guild_id, user_id)

    def add_abort_submission(
        self, guild_id: int | None, user_id: int, is_admin: bool,
        pending_match_id: int | None = None
    ) -> dict[str, Any]:
        """Add a vote to abort the match (delegates to voting_service)."""
        return self.voting_service.add_abort_submission(guild_id, user_id, is_admin, pending_match_id)

    def get_vote_counts(self, guild_id: int | None, pending_match_id: int | None = None) -> dict[str, int]:
        """Get vote counts for radiant and dire (delegates to voting_service)."""
        return self.voting_service.get_vote_counts(guild_id, pending_match_id)

    def get_pending_record_result(self, guild_id: int | None, pending_match_id: int | None = None) -> str | None:
        """Get the result to record if threshold met (delegates to voting_service)."""
        return self.voting_service.get_pending_record_result(guild_id, pending_match_id)

    def can_record_match(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if there are enough votes to record (delegates to voting_service)."""
        return self.voting_service.can_record_match(guild_id, pending_match_id)

    def correct_match_result(
        self,
        match_id: int,
        new_winning_team: str,
        guild_id: int | None = None,
        corrected_by: int | None = None,
    ) -> dict:
        """
        Correct an incorrectly recorded match result.

        This reverses all effects of the original recording and re-applies
        them with the correct winning team. Effects reversed/reapplied:
        - Win/loss counters
        - Glicko-2 ratings (restored from rating_history snapshots, with
          streak multipliers recomputed for the corrected result)
        - OpenSkill ratings (restored from rating_history snapshots)
        - Bet payouts (winners become losers and vice versa)
        - JC win bonuses (old winners debited, corrected winners awarded)
        - Pairings statistics

        Note: Loan repayments are NOT reversed (they are deferred payments,
        not match-dependent rewards). Participation bonuses are not swapped
        either: the demoted winners never received the losers' participation
        JC, and clawing the promoted winners' back would punish them for
        playing.

        Args:
            match_id: The match ID to correct
            new_winning_team: 'radiant' or 'dire'
            guild_id: Guild ID (for bet operations)
            corrected_by: Discord ID of the admin making the correction

        Returns:
            Dict with correction details and summary

        Raises:
            ValueError: If match not found, result unchanged, or missing data
        """
        if new_winning_team not in ("radiant", "dire"):
            raise ValueError("new_winning_team must be 'radiant' or 'dire'")

        # 1. Load match data
        match = self.match_repo.get_match(match_id, guild_id)
        if not match:
            raise ValueError(f"Match {match_id} not found")

        old_winning_team_num = match.get("winning_team")  # 1 = Radiant, 2 = Dire
        new_winning_team_num = 1 if new_winning_team == "radiant" else 2

        if old_winning_team_num == new_winning_team_num:
            raise ValueError(
                f"Match {match_id} already has {new_winning_team} as winner"
            )

        old_winning_team = "radiant" if old_winning_team_num == 1 else "dire"

        # 2. Get participant data
        participants = self.match_repo.get_match_participants(match_id, guild_id)
        if not participants:
            raise ValueError(f"No participants found for match {match_id}")

        radiant_ids = [p["discord_id"] for p in participants if p.get("side") == "radiant"]
        dire_ids = [p["discord_id"] for p in participants if p.get("side") == "dire"]

        if len(radiant_ids) != 5 or len(dire_ids) != 5:
            logger.warning(
                f"Match {match_id} correction: unexpected team sizes "
                f"radiant={len(radiant_ids)}, dire={len(dire_ids)}"
            )

        # Determine old/new winners and losers
        old_winner_ids = radiant_ids if old_winning_team == "radiant" else dire_ids
        old_loser_ids = dire_ids if old_winning_team == "radiant" else radiant_ids
        new_winner_ids = radiant_ids if new_winning_team == "radiant" else dire_ids
        new_loser_ids = dire_ids if new_winning_team == "radiant" else radiant_ids

        # 3. Load rating history for restoration
        rating_history = self.match_repo.get_full_rating_history_for_match(match_id)
        if not rating_history:
            raise ValueError(
                f"No rating history found for match {match_id}. "
                "Cannot correct matches without stored snapshots."
            )

        # 4. Compute the new ratings (pure — reads only).
        # Build Glicko players from restored pre-match ratings in rating_history;
        # fall back to current rating if a snapshot is missing.
        rating_by_id = {e["discord_id"]: e for e in rating_history}

        # Recompute per-player streak multipliers as-of this match with the
        # CORRECTED result. Recording amplifies Glicko deltas on 3+ game
        # streaks; a correction that ignored them produced different ratings
        # than recording the true result would have. The pre-match outcome
        # window is recoverable from rating_history (rows strictly before
        # this match), so the multiplier is recomputed, not restored.
        streak_multipliers: dict[int, float] = {}
        new_streaks: dict[int, tuple[int, float]] = {}
        if hasattr(self.match_repo, "get_player_outcomes_before_match"):
            for pid in radiant_ids + dire_ids:
                outcomes = self.match_repo.get_player_outcomes_before_match(
                    pid, guild_id, match_id, limit=20
                )
                slen, mult = self.rating_system.calculate_streak_multiplier(
                    outcomes, won=pid in new_winner_ids
                )
                streak_multipliers[pid] = mult
                new_streaks[pid] = (slen, mult)

        def _build_player(pid: int):
            entry = rating_by_id.get(pid)
            if entry and entry.get("rating_before") is not None:
                return self.rating_system.create_player_from_rating(
                    entry["rating_before"],
                    entry["rd_before"],
                    entry["volatility_before"],
                )
            return self._load_glicko_player(pid, guild_id)[0]

        radiant_glicko = [(_build_player(pid), pid) for pid in radiant_ids]
        dire_glicko = [(_build_player(pid), pid) for pid in dire_ids]

        if new_winning_team == "radiant":
            team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                radiant_glicko, dire_glicko, 1, streak_multipliers=streak_multipliers
            )
        else:
            team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                dire_glicko, radiant_glicko, 1, streak_multipliers=streak_multipliers
            )

        new_glicko_updates = [
            (pid, rating, rd, vol) for rating, rd, vol, pid in team1_updated + team2_updated
        ]
        glicko_by_id = {pid: (rating, rd, vol) for pid, rating, rd, vol in new_glicko_updates}

        # 5. Recompute OpenSkill if we have baseline snapshots.
        os_rating_by_id = {
            e["discord_id"]: (e["os_mu_before"], e["os_sigma_before"])
            for e in rating_history
            if e.get("os_mu_before") is not None
        }
        new_os_updates: list[tuple[int, float, float]] = []
        os_results: dict[int, tuple[float, float]] = {}
        if os_rating_by_id:
            radiant_os_data = [
                (pid, *os_rating_by_id.get(pid, (None, None))) for pid in radiant_ids
            ]
            dire_os_data = [
                (pid, *os_rating_by_id.get(pid, (None, None))) for pid in dire_ids
            ]
            os_results = self.openskill_system.update_ratings_equal_weight(
                radiant_os_data, dire_os_data,
                winning_team=new_winning_team_num,
            )
            new_os_updates = [(pid, mu, sigma) for pid, (mu, sigma) in os_results.items()]

        # 6. Build rating_history correction rows.
        rating_history_updates: list[dict] = []
        for entry in rating_history:
            pid = entry["discord_id"]
            new_rating, new_rd, new_vol = glicko_by_id.get(pid, (None, None, None))
            if new_rating is None:
                continue
            update = {
                "discord_id": pid,
                "new_rating": new_rating,
                "new_rd": new_rd,
                "new_volatility": new_vol,
                "new_won": pid in new_winner_ids,
            }
            if pid in os_results:
                new_mu, new_sigma = os_results[pid]
                update["new_os_mu"] = new_mu
                update["new_os_sigma"] = new_sigma
            if pid in new_streaks:
                update["new_streak_length"] = new_streaks[pid][0]
                update["new_streak_multiplier"] = new_streaks[pid][1]
            rating_history_updates.append(update)

        # 7. Pre-compute bet correction deltas outside the atomic block. Bet
        # payout reversal/re-apply writes to the bets table via bet_repo and
        # returns per-player balance deltas; we apply those after the core
        # match invariant commits.
        bet_correction_summary = {}
        combined_bet_deltas: dict[int, int] = {}
        old_winning_bets: list = []
        new_winning_bets: list = []
        if self.betting_service and hasattr(self.betting_service, "bet_repo"):
            bet_repo = self.betting_service.bet_repo
            all_bets = bet_repo.get_settled_bets_for_match(match_id)

            if all_bets:
                old_winning_bets = [
                    b for b in all_bets
                    if (old_winning_team == "radiant" and b["team_bet_on"] == "radiant")
                    or (old_winning_team == "dire" and b["team_bet_on"] == "dire")
                ]
                new_winning_bets = [
                    b for b in all_bets
                    if (new_winning_team == "radiant" and b["team_bet_on"] == "radiant")
                    or (new_winning_team == "dire" and b["team_bet_on"] == "dire")
                ]

        # 8. Atomic core: swap wins/losses, apply new ratings, rewrite history,
        # flip matches.winning_team, re-apply pairings, and log the correction
        # — all in one BEGIN IMMEDIATE. Bet settlement stays outside.
        correction_id = self.match_repo.correct_match_result_atomic(
            match_id=match_id,
            guild_id=guild_id,
            old_winning_team=old_winning_team_num,
            new_winning_team=new_winning_team_num,
            old_winner_ids=old_winner_ids,
            old_loser_ids=old_loser_ids,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            glicko_updates=new_glicko_updates,
            openskill_updates=new_os_updates,
            rating_history_updates=rating_history_updates,
            corrected_by=corrected_by,
        )

        # 9. Bet payout correction: reverse old winners, pay new winners, and
        # apply the combined balance deltas — all folded into ONE atomic
        # transaction inside bet_repo so the bets-table payout rewrite and the
        # player-balance credits commit-or-rollback together. If this raises,
        # nothing is half-applied; the match state is correct and the admin can
        # rerun the correction flow. Uses the original betting_mode persisted on
        # the match row so a match recorded in house mode is corrected with
        # house-mode math (older pre-migration matches default to 'pool').
        if self.betting_service and (old_winning_bets or new_winning_bets):
            bet_repo = self.betting_service.bet_repo
            all_bets_len = len(old_winning_bets) + len(new_winning_bets)
            original_betting_mode = match.get("betting_mode") or "pool"
            combined_bet_deltas = bet_repo.settle_bet_correction_atomic(
                match_id,
                old_winning_bets,
                new_winning_bets,
                guild_id,
                pool_mode=(original_betting_mode == "pool"),
            )
            bet_correction_summary = {
                "bets_affected": all_bets_len,
                "old_winners_reversed": len(old_winning_bets),
                "new_winners_paid": len(new_winning_bets),
                "balance_changes": combined_bet_deltas,
            }

        # 10. JC win-bonus correction: the old "winners" keep their win bonus
        # and the corrected winners never got theirs unless it moves here.
        # The reversal debits each old winner's recorded win-bonus balance
        # delta (win_bonus_jc, snapshotted at recording) in one atomic txn;
        # matches recorded before the snapshot existed fall back to the gross
        # JOPACOIN_WIN_REWARD — the best recoverable figure, since garnishment
        # stayed in the balance and any bankruptcy penalty withheld back then
        # was never persisted per-player. The re-award goes through the
        # existing award primitive (so current garnishment / bankruptcy rules
        # apply) and its delta is snapshotted so a repeat correction reverses
        # exactly what was paid.
        win_bonus_correction: dict = {}
        if self.betting_service and hasattr(self.match_repo, "apply_win_bonus_reversal_atomic"):
            from config import JOPACOIN_WIN_REWARD

            participants_by_id = {p["discord_id"]: p for p in participants}
            win_bonus_debits: dict[int, int] = {}
            for pid in old_winner_ids:
                stored = participants_by_id.get(pid, {}).get("win_bonus_jc")
                amount = int(stored) if stored is not None else JOPACOIN_WIN_REWARD
                if amount > 0:
                    win_bonus_debits[pid] = amount
            self.match_repo.apply_win_bonus_reversal_atomic(
                match_id, guild_id, win_bonus_debits
            )

            new_win_awards = self.betting_service.award_win_bonus(new_winner_ids, guild_id)
            win_bonus_awarded = {
                pid: int(r.get("net", 0)) + int(r.get("garnished", 0))
                for pid, r in new_win_awards.items()
            }
            if win_bonus_awarded and hasattr(self.match_repo, "update_participant_bonus_jc"):
                self.match_repo.update_participant_bonus_jc(
                    match_id, guild_id, {}, win_bonus_by_player=win_bonus_awarded
                )
            win_bonus_correction = {
                "reversed": win_bonus_debits,
                "awarded": win_bonus_awarded,
            }

        logger.info(
            f"Match {match_id} corrected: {old_winning_team} -> {new_winning_team} "
            f"(by user {corrected_by})"
        )

        return {
            "match_id": match_id,
            "old_winning_team": old_winning_team,
            "new_winning_team": new_winning_team,
            "correction_id": correction_id,
            "players_affected": len(radiant_ids) + len(dire_ids),
            "ratings_updated": len(new_glicko_updates),
            "bet_correction": bet_correction_summary,
            "win_bonus_correction": win_bonus_correction,
            "new_winner_ids": new_winner_ids,
            "new_loser_ids": new_loser_ids,
        }
