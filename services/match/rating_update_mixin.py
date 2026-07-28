"""RatingUpdateMixin mixin for :class:`MatchService`.

OpenSkill rating maintenance: the performance-adjusted Phase 2 update, the
all-matches replay (performance and equal-contribution paths), and OpenSkill
win-probability prediction.

Mixin split out of the former monolithic ``match_service`` module; it carries
no state of its own and is composed into ``MatchService``.
"""

from services.match._common import logger


class RatingUpdateMixin:
    """RatingUpdateMixin — see module docstring.

    Composed into :class:`~services.match_service.MatchService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    def update_openskill_ratings_for_match(
        self, match_id: int, guild_id: int | None = None
    ) -> dict:
        """
        Update OpenSkill ratings with bounded native contribution weights.

        This method should be called AFTER match enrichment when fantasy_points
        have been calculated and stored in match_participants.

        Args:
            match_id: The internal match ID to update ratings for
            guild_id: Guild ID for multi-guild support

        Returns:
            Dict with:
            - success: bool
            - players_updated: int
            - players_skipped: int (missing fantasy data)
            - error: str (if failed)
        """
        # Get match data
        match = self.match_repo.get_match(match_id, guild_id)
        if not match:
            return {
                "success": False,
                "error": f"Match {match_id} not found",
                "players_updated": 0,
                "players_skipped": 0,
            }

        winning_team = match.get("winning_team")  # 1 = Radiant, 2 = Dire
        if winning_team not in (1, 2):
            return {
                "success": False,
                "error": f"Invalid winning_team: {winning_team}",
                "players_updated": 0,
                "players_skipped": 0,
            }

        # Get participants with fantasy points
        participants = self.match_repo.get_match_participants(match_id, guild_id)
        if not participants:
            return {
                "success": False,
                "error": "No participants found for match",
                "players_updated": 0,
                "players_skipped": 0,
            }

        # Separate by team
        radiant = [p for p in participants if p.get("side") == "radiant"]
        dire = [p for p in participants if p.get("side") == "dire"]

        if len(radiant) != 5 or len(dire) != 5:
            logger.warning(
                f"Match {match_id}: unexpected team sizes radiant={len(radiant)}, dire={len(dire)}"
            )

        # A partial payload would compare real scores with the default factor
        # and manufacture a performance extreme. Phase 2 is all-or-nothing.
        has_complete_fantasy = len(participants) == 10 and all(
            p.get("fantasy_points") is not None for p in participants
        )
        if not has_complete_fantasy:
            logger.info(f"Match {match_id}: incomplete fantasy data, skipping OpenSkill update")
            return {
                "success": True,
                "players_updated": 0,
                "players_skipped": len(participants),
                "reason": "Complete fantasy data is required for all 10 players",
            }

        # Recompute the complete chain under one database write lock. This is
        # fast for the league's history and avoids both partial Phase-2 commits
        # and a race with a newly recorded match.
        mark_pending = getattr(
            self.match_repo,
            "mark_openskill_replay_pending",
            None,
        )
        if callable(mark_pending):
            mark_pending(
                guild_id if guild_id is not None else 0,
                f"match_enrichment:{match_id}",
            )
        replay = self.backfill_openskill_ratings(
            guild_id=guild_id,
            reset_first=True,
        )
        if replay["errors"]:
            return {
                "success": False,
                "error": "OpenSkill replay failed: " + "; ".join(replay["errors"]),
                "players_updated": 0,
                "players_skipped": len(participants),
            }
        updated_count = len(participants)

        logger.info(
            f"OpenSkill update complete for match {match_id}: {updated_count} players updated"
        )

        return {
            "success": True,
            "players_updated": updated_count,
            "players_skipped": len(participants) - updated_count,
        }

    def backfill_openskill_ratings(
        self, guild_id: int | None = None, reset_first: bool = True
    ) -> dict:
        """
        Replay OpenSkill ratings from ALL matches.

        Processes matches in chronological order to simulate rating progression.
        - Enriched matches: use bounded native performance weights
        - Non-enriched matches: use equal contribution

        Args:
            reset_first: If True, reset all players' OpenSkill ratings to defaults before backfill

        Returns:
            Dict with:
            - matches_processed: int
            - matches_with_fantasy: int
            - matches_equal_weight: int
            - players_updated: int (unique players)
            - errors: list of error messages
        """
        if not reset_first:
            raise ValueError(
                "reset_first=False is unsupported because it reapplies complete "
                "history on top of already-final ratings"
            )
        logger.info("Starting atomic OpenSkill history replay...")
        normalized_guild = guild_id if guild_id is not None else 0
        replay, total_matches = self.match_repo.replay_openskill_atomic(
            guild_id=normalized_guild,
            system=self.openskill_system,
        )
        logger.info(f"Found {total_matches} total matches to process")

        if total_matches == 0:
            return {
                "matches_processed": 0,
                "matches_with_fantasy": 0,
                "matches_equal_weight": 0,
                "players_updated": len(replay.final_ratings),
                "total_matches": 0,
                "errors": [],
            }

        if replay.errors:
            logger.error(
                "OpenSkill replay aborted before persistence: %s",
                "; ".join(replay.errors[:5]),
            )
            return replay.summary(total_matches)

        logger.info(
            f"OpenSkill replay complete: {replay.matches_processed} matches "
            f"({replay.matches_with_fantasy} performance, "
            f"{replay.matches_equal_weight} equal-weight), "
            f"{len(replay.players_touched)} unique players"
        )
        return replay.summary(total_matches)

    def get_openskill_predictions_for_match(
        self, team1_ids: list[int], team2_ids: list[int], guild_id: int | None = None
    ) -> dict:
        """
        Get OpenSkill predicted win probability for a match.

        Args:
            team1_ids: Discord IDs for team 1 (Radiant)
            team2_ids: Discord IDs for team 2 (Dire)

        Returns:
            Dict with calibrated team1_win_prob, raw_team1_win_prob,
            team1_ordinal, team2_ordinal.
        """

        # Get current ratings
        all_ids = team1_ids + team2_ids
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids, guild_id)

        # Build ratings for each team. Use the same OpenSkill probability model as
        # shuffle-time previews instead of a separate ordinal logistic approximation.
        team1_ratings = []
        team1_ordinals = []
        for pid in team1_ids:
            mu, sigma = os_ratings.get(pid, (None, None))
            actual_mu = mu if mu is not None else self.openskill_system.DEFAULT_MU
            actual_sigma = sigma if sigma is not None else self.openskill_system.DEFAULT_SIGMA
            team1_ratings.append((actual_mu, actual_sigma))
            team1_ordinals.append(self.openskill_system.ordinal(actual_mu, actual_sigma))

        team2_ratings = []
        team2_ordinals = []
        for pid in team2_ids:
            mu, sigma = os_ratings.get(pid, (None, None))
            actual_mu = mu if mu is not None else self.openskill_system.DEFAULT_MU
            actual_sigma = sigma if sigma is not None else self.openskill_system.DEFAULT_SIGMA
            team2_ratings.append((actual_mu, actual_sigma))
            team2_ordinals.append(self.openskill_system.ordinal(actual_mu, actual_sigma))

        team1_avg_ordinal = sum(team1_ordinals) / len(team1_ordinals) if team1_ordinals else 0
        team2_avg_ordinal = sum(team2_ordinals) / len(team2_ordinals) if team2_ordinals else 0

        raw_team1_win_prob = self.openskill_system.os_predict_win_probability(
            team1_ratings, team2_ratings
        )
        team1_win_prob = self.openskill_system.calibrate_win_probability(raw_team1_win_prob)

        return {
            "team1_win_prob": team1_win_prob,
            "raw_team1_win_prob": raw_team1_win_prob,
            "team1_avg_ordinal": team1_avg_ordinal,
            "team2_avg_ordinal": team2_avg_ordinal,
        }
