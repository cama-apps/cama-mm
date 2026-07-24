"""RatingUpdateMixin mixin for :class:`MatchService`.

OpenSkill rating maintenance: the fantasy-weighted Phase 2 update, the
all-matches backfill (FP-weighted and equal-weight paths), the rating_history
OpenSkill patch helper, and the OpenSkill win-probability prediction.

Mixin split out of the former monolithic ``match_service`` module; it carries
no state of its own and is composed into ``MatchService``.
"""

from services.match._common import coalesce_os_baseline, logger


class RatingUpdateMixin:
    """RatingUpdateMixin — see module docstring.

    Composed into :class:`~services.match_service.MatchService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    def update_openskill_ratings_for_match(self, match_id: int, guild_id: int | None = None) -> dict:
        """
        Update OpenSkill ratings for a match using fantasy points as weights.

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

        # Check if any participants have fantasy data
        has_fantasy = any(p.get("fantasy_points") is not None for p in participants)
        if not has_fantasy:
            logger.info(f"Match {match_id}: no fantasy data available, skipping OpenSkill update")
            return {
                "success": True,
                "players_updated": 0,
                "players_skipped": len(participants),
                "reason": "No fantasy data available",
            }

        # === Phase 2: Get baseline from rating_history (Phase 1 values) ===
        # This retrieves the pre-match OpenSkill ratings that were stored during
        # Phase 1, allowing us to recalculate with fantasy weights from the same
        # starting point.
        discord_ids = [p["discord_id"] for p in participants]
        os_baseline = self.match_repo.get_os_baseline_for_match(match_id, guild_id)

        if os_baseline:
            # Use Phase 1 baseline (os_mu_before/os_sigma_before from rating_history).
            # A participant missing from a partial baseline must fall back to their
            # CURRENT rating, not OpenSkill defaults — otherwise a real rating would
            # be recomputed from mu=25 and silently regress.
            os_ratings = dict(os_baseline)
            missing = [d for d in discord_ids if d not in os_ratings]
            if missing:
                os_ratings.update(
                    self.player_repo.get_openskill_ratings_bulk(missing, guild_id)
                )
            logger.debug(f"Match {match_id}: using Phase 1 baseline for {len(os_baseline)} players")
        else:
            # Legacy fallback: use current player ratings (pre-Phase 1 matches)
            os_ratings = self.player_repo.get_openskill_ratings_bulk(discord_ids, guild_id)
            logger.debug(f"Match {match_id}: no Phase 1 baseline, using current ratings")

        # Build team data for OpenSkill update
        # Format: (discord_id, mu, sigma, fantasy_points)
        team1_data = []  # Radiant
        team2_data = []  # Dire

        for p in radiant:
            discord_id = p["discord_id"]
            mu, sigma = os_ratings.get(discord_id, (None, None))
            fantasy_points = p.get("fantasy_points")
            team1_data.append((discord_id, mu, sigma, fantasy_points))

        for p in dire:
            discord_id = p["discord_id"]
            mu, sigma = os_ratings.get(discord_id, (None, None))
            fantasy_points = p.get("fantasy_points")
            team2_data.append((discord_id, mu, sigma, fantasy_points))

        # Run OpenSkill update
        try:
            results = self.openskill_system.update_ratings_after_match(
                team1_data=team1_data,
                team2_data=team2_data,
                winning_team=winning_team,
            )
        except Exception as e:
            logger.error(f"OpenSkill update failed for match {match_id}: {e}")
            return {
                "success": False,
                "error": str(e),
                "players_updated": 0,
                "players_skipped": 0,
            }

        # Persist ratings + rating_history together so a crash can't leave
        # players.os_* updated without the matching rating_history row
        # (or vice versa). Fantasy weights must track the OS values we just
        # committed to players.
        updates = [(pid, mu, sigma) for pid, (mu, sigma, _) in results.items()]
        history_updates = []
        default_mu = self.openskill_system.DEFAULT_MU
        default_sigma = self.openskill_system.DEFAULT_SIGMA
        for pid, (new_mu, new_sigma, fantasy_weight) in results.items():
            old_mu, old_sigma = os_ratings.get(pid, (None, None))
            os_mu_before, os_sigma_before = coalesce_os_baseline(
                old_mu, old_sigma, default_mu, default_sigma
            )
            history_updates.append({
                "discord_id": pid,
                "os_mu_before": os_mu_before,
                "os_mu_after": new_mu,
                "os_sigma_before": os_sigma_before,
                "os_sigma_after": new_sigma,
                "fantasy_weight": fantasy_weight,
            })

        result = self.match_repo.apply_openskill_phase2_atomic(
            match_id=match_id,
            guild_id=guild_id,
            player_updates=updates,
            history_updates=history_updates,
        )
        updated_count = result["players_updated"]
        history_updated = result["history_updated"]

        if history_updates and history_updated < len(history_updates):
            logger.warning(
                f"Only {history_updated}/{len(history_updates)} rating_history entries found for match {match_id}"
            )

        logger.info(
            f"OpenSkill update complete for match {match_id}: {updated_count} players updated"
        )

        return {
            "success": True,
            "players_updated": updated_count,
            "players_skipped": len(participants) - updated_count,
        }


    def backfill_openskill_ratings(self, guild_id: int | None = None, reset_first: bool = True) -> dict:
        """
        Backfill OpenSkill ratings from ALL matches.

        Processes matches in chronological order to simulate rating progression.
        - Enriched matches (with fantasy data): Use FP-weighted update with blending
        - Non-enriched matches: Use equal-weight update

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
        logger.info("Starting OpenSkill backfill (all matches)...")

        errors = []
        matches_processed = 0
        matches_with_fantasy = 0
        matches_equal_weight = 0
        players_touched = set()

        # Get ALL matches in chronological order (not just enriched)
        normalized_guild = guild_id if guild_id is not None else 0
        all_matches = self.match_repo.get_all_matches_chronological(normalized_guild)
        total_matches = len(all_matches)
        logger.info(f"Found {total_matches} total matches to process")

        if total_matches == 0:
            return {
                "matches_processed": 0,
                "matches_with_fantasy": 0,
                "matches_equal_weight": 0,
                "players_updated": 0,
                "errors": ["No matches found"],
            }

        participants_by_match = self.match_repo.get_match_participants_bulk(
            [match["match_id"] for match in all_matches], normalized_guild
        )

        # Replay against one in-memory rating snapshot. Match order still
        # determines every update, but no per-match database reads or writes
        # are needed. Persist the completed snapshot once at the end.
        all_players = self.player_repo.get_all(normalized_guild)
        current_os_ratings: dict[
            int, tuple[float | None, float | None]
        ] = {}
        for player in all_players:
            if player.discord_id is None:
                continue
            if reset_first:
                seed_mu = (
                    self.openskill_system.mmr_to_os_mu(
                        self.rating_system.new_player_seed_mmr(player.initial_mmr)
                    )
                    if player.initial_mmr is not None
                    else self.openskill_system.DEFAULT_MU
                )
                current_os_ratings[player.discord_id] = (
                    seed_mu,
                    self.openskill_system.DEFAULT_SIGMA,
                )
            else:
                current_os_ratings[player.discord_id] = (
                    player.os_mu,
                    player.os_sigma,
                )

        # Process each match in chronological order
        for i, match in enumerate(all_matches):
            match_id = match["match_id"]
            winning_team = match["winning_team"]

            try:
                # Get participants to check for fantasy data
                participants = participants_by_match[match_id]

                if not participants:
                    # No participants recorded - use team lists from match
                    radiant_ids = match.get("team1_players", [])
                    dire_ids = match.get("team2_players", [])
                    if not radiant_ids or not dire_ids:
                        errors.append(f"Match {match_id}: No participant data")
                        continue
                    has_fantasy = False
                else:
                    radiant_ids = [p["discord_id"] for p in participants if p.get("side") == "radiant"]
                    dire_ids = [p["discord_id"] for p in participants if p.get("side") == "dire"]
                    has_fantasy = any(p.get("fantasy_points") is not None for p in participants)

                    # If no side info, fall back to match team lists and use equal weight
                    # (can't use fantasy weights without knowing which player was on which team)
                    if not radiant_ids or not dire_ids:
                        radiant_ids = match.get("team1_players", [])
                        dire_ids = match.get("team2_players", [])
                        has_fantasy = False

                if has_fantasy:
                    # Use FP-weighted update (with blending)
                    result = self._backfill_match_with_fantasy(
                        participants,
                        winning_team,
                        current_os_ratings,
                    )
                    if result.get("success"):
                        matches_with_fantasy += 1
                else:
                    # Use equal-weight update
                    result = self._backfill_match_equal_weight(
                        radiant_ids,
                        dire_ids,
                        winning_team,
                        current_os_ratings,
                    )
                    if result.get("success"):
                        matches_equal_weight += 1

                if result.get("success"):
                    matches_processed += 1
                    for pid in radiant_ids + dire_ids:
                        players_touched.add(pid)
                else:
                    error = result.get("error", "Unknown error")
                    errors.append(f"Match {match_id}: {error}")

            except Exception as e:
                errors.append(f"Match {match_id}: {str(e)}")
                logger.error(f"Backfill error for match {match_id}: {e}")

            # Log progress periodically
            if (i + 1) % 50 == 0 or (i + 1) == total_matches:
                logger.info(f"Backfill progress: {i + 1}/{total_matches} matches processed")

        persist_ids = set(current_os_ratings) if reset_first else players_touched
        final_updates = []
        for player_id in sorted(persist_ids):
            rating = current_os_ratings.get(player_id)
            if rating is None:
                continue
            mu, sigma = rating
            if mu is not None and sigma is not None:
                final_updates.append((player_id, mu, sigma))
        if final_updates:
            try:
                self.player_repo.update_openskill_ratings_bulk(
                    final_updates, normalized_guild
                )
            except Exception as exc:
                errors.append(f"Failed to persist backfill ratings: {exc}")
                logger.error(f"Failed to persist OpenSkill backfill ratings: {exc}")

        logger.info(
            f"OpenSkill backfill complete: {matches_processed} matches "
            f"({matches_with_fantasy} FP-weighted, {matches_equal_weight} equal-weight), "
            f"{len(players_touched)} unique players"
        )

        return {
            "matches_processed": matches_processed,
            "matches_with_fantasy": matches_with_fantasy,
            "matches_equal_weight": matches_equal_weight,
            "players_updated": len(players_touched),
            "total_matches": total_matches,
            "errors": errors[:10],  # Limit error list
        }

    def _backfill_match_with_fantasy(
        self,
        participants: list[dict],
        winning_team: int,
        os_ratings: dict[int, tuple[float | None, float | None]],
    ) -> dict:
        """
        Backfill a single match using FP-weighted OpenSkill update.

        Uses the in-memory replay ratings and fantasy points from participants.
        """
        radiant = [p for p in participants if p.get("side") == "radiant"]
        dire = [p for p in participants if p.get("side") == "dire"]

        if len(radiant) != 5 or len(dire) != 5:
            return {"success": False, "error": f"Invalid team sizes: {len(radiant)}/{len(dire)}"}

        # Build team data: (discord_id, mu, sigma, fantasy_points)
        team1_data = []
        for p in radiant:
            pid = p["discord_id"]
            mu, sigma = os_ratings.get(pid, (None, None))
            fp = p.get("fantasy_points")
            team1_data.append((pid, mu, sigma, fp))

        team2_data = []
        for p in dire:
            pid = p["discord_id"]
            mu, sigma = os_ratings.get(pid, (None, None))
            fp = p.get("fantasy_points")
            team2_data.append((pid, mu, sigma, fp))

        try:
            results = self.openskill_system.update_ratings_after_match(
                team1_data, team2_data, winning_team
            )
            os_ratings.update(
                {
                    player_id: (mu, sigma)
                    for player_id, (mu, sigma, _) in results.items()
                }
            )
            return {"success": True, "players_updated": len(results)}
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _backfill_match_equal_weight(
        self,
        radiant_ids: list[int],
        dire_ids: list[int],
        winning_team: int,
        os_ratings: dict[int, tuple[float | None, float | None]],
    ) -> dict:
        """
        Backfill a single match using equal-weight OpenSkill update.

        Used for non-enriched matches without fantasy data.
        """
        if len(radiant_ids) != 5 or len(dire_ids) != 5:
            return {"success": False, "error": f"Invalid team sizes: {len(radiant_ids)}/{len(dire_ids)}"}

        # Build team data: (discord_id, mu, sigma)
        radiant_data = [
            (pid, *os_ratings.get(pid, (None, None)))
            for pid in radiant_ids
        ]
        dire_data = [
            (pid, *os_ratings.get(pid, (None, None)))
            for pid in dire_ids
        ]

        try:
            results = self.openskill_system.update_ratings_equal_weight(
                radiant_data, dire_data, winning_team
            )
            os_ratings.update(results)
            return {"success": True, "players_updated": len(results)}
        except Exception as e:
            return {"success": False, "error": str(e)}

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
