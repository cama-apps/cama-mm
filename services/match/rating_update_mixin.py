"""RatingUpdateMixin mixin for :class:`MatchService`.

OpenSkill rating maintenance: the fantasy-weighted Phase 2 update, the
all-matches backfill (FP-weighted and equal-weight paths), the rating_history
OpenSkill patch helper, and the OpenSkill win-probability prediction.

Mixin split out of the former monolithic ``match_service`` module; it carries
no state of its own and is composed into ``MatchService``.
"""

from services.match._common import logger


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
        os_baseline = self.match_repo.get_os_baseline_for_match(match_id)

        if os_baseline:
            # Use Phase 1 baseline (os_mu_before/os_sigma_before from rating_history)
            os_ratings = os_baseline
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
        for pid, (new_mu, new_sigma, fantasy_weight) in results.items():
            old_mu, old_sigma = os_ratings.get(pid, (None, None))
            history_updates.append({
                "discord_id": pid,
                "os_mu_before": old_mu,
                "os_mu_after": new_mu,
                "os_sigma_before": old_sigma,
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

    def _update_rating_history_openskill(
        self,
        match_id: int,
        discord_id: int,
        os_mu_before: float | None,
        os_mu_after: float,
        os_sigma_before: float | None,
        os_sigma_after: float,
        fantasy_weight: float | None,
    ) -> None:
        """
        Update an existing rating_history entry with OpenSkill data.

        If no existing entry exists, this is a no-op (OpenSkill updates happen
        after enrichment, so rating_history should already exist from record_match).
        """
        updated = self.match_repo.update_rating_history_openskill(
            match_id=match_id,
            discord_id=discord_id,
            os_mu_before=os_mu_before,
            os_mu_after=os_mu_after,
            os_sigma_before=os_sigma_before,
            os_sigma_after=os_sigma_after,
            fantasy_weight=fantasy_weight,
        )
        if not updated:
            logger.warning(
                f"No rating_history entry found for match {match_id}, player {discord_id}"
            )

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

        # Reset all players' OpenSkill ratings if requested
        # Seed from initial_mmr (what they started with), falling back to DEFAULT_MU
        if reset_first:
            all_players = self.player_repo.get_all(normalized_guild)
            reset_updates = []
            for p in all_players:
                if p.discord_id is None:
                    continue
                # Seed mu from initial_mmr (OpenDota MMR at registration)
                if p.initial_mmr is not None:
                    # Convert MMR to mu: mu = 25 + (mmr / 200)
                    seed_mu = self.openskill_system.mmr_to_os_mu(p.initial_mmr)
                else:
                    seed_mu = self.openskill_system.DEFAULT_MU
                reset_updates.append((p.discord_id, seed_mu, self.openskill_system.DEFAULT_SIGMA))
            if reset_updates:
                self.player_repo.update_openskill_ratings_bulk(reset_updates, normalized_guild)
                logger.info(f"Reset {len(reset_updates)} players to seeded OpenSkill ratings")

        # Process each match in chronological order
        for i, match in enumerate(all_matches):
            match_id = match["match_id"]
            winning_team = match["winning_team"]

            try:
                # Get participants to check for fantasy data
                participants = self.match_repo.get_match_participants(match_id, normalized_guild)

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

                # Get guild_id from match for per-guild updates
                match_guild_id = match.get("guild_id")

                if has_fantasy:
                    # Use FP-weighted update (with blending)
                    result = self._backfill_match_with_fantasy(match_id, match_guild_id, participants, winning_team)
                    if result.get("success"):
                        matches_with_fantasy += 1
                else:
                    # Use equal-weight update
                    result = self._backfill_match_equal_weight(
                        match_id, match_guild_id, radiant_ids, dire_ids, winning_team
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
        match_id: int,
        guild_id: int | None,
        participants: list[dict],
        winning_team: int,
    ) -> dict:
        """
        Backfill a single match using FP-weighted OpenSkill update.

        Uses current player ratings (after reset) and fantasy points from participants.
        """
        radiant = [p for p in participants if p.get("side") == "radiant"]
        dire = [p for p in participants if p.get("side") == "dire"]

        if len(radiant) != 5 or len(dire) != 5:
            return {"success": False, "error": f"Invalid team sizes: {len(radiant)}/{len(dire)}"}

        # Get current ratings (from DB, after potential reset)
        all_ids = [p["discord_id"] for p in participants]
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids, guild_id)

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
            # Persist updated ratings
            updates = [(pid, mu, sigma) for pid, (mu, sigma, _) in results.items()]
            self.player_repo.update_openskill_ratings_bulk(updates, guild_id)
            return {"success": True, "players_updated": len(updates)}
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _backfill_match_equal_weight(
        self,
        match_id: int,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        winning_team: int,
    ) -> dict:
        """
        Backfill a single match using equal-weight OpenSkill update.

        Used for non-enriched matches without fantasy data.
        """
        if len(radiant_ids) != 5 or len(dire_ids) != 5:
            return {"success": False, "error": f"Invalid team sizes: {len(radiant_ids)}/{len(dire_ids)}"}

        # Get current ratings (from DB, after potential reset)
        all_ids = radiant_ids + dire_ids
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids, guild_id)

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
            # Persist updated ratings
            updates = [(pid, mu, sigma) for pid, (mu, sigma) in results.items()]
            self.player_repo.update_openskill_ratings_bulk(updates, guild_id)
            return {"success": True, "players_updated": len(updates)}
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
            Dict with team1_win_prob, team1_ordinal, team2_ordinal
        """

        # Get current ratings
        all_ids = team1_ids + team2_ids
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids, guild_id)

        # Build ratings for each team
        team1_ratings = []
        team1_ordinals = []
        for pid in team1_ids:
            mu, sigma = os_ratings.get(pid, (None, None))
            rating = self.openskill_system.create_rating(mu, sigma)
            team1_ratings.append(rating)
            actual_mu = mu if mu is not None else self.openskill_system.DEFAULT_MU
            actual_sigma = sigma if sigma is not None else self.openskill_system.DEFAULT_SIGMA
            team1_ordinals.append(self.openskill_system.ordinal(actual_mu, actual_sigma))

        team2_ratings = []
        team2_ordinals = []
        for pid in team2_ids:
            mu, sigma = os_ratings.get(pid, (None, None))
            rating = self.openskill_system.create_rating(mu, sigma)
            team2_ratings.append(rating)
            actual_mu = mu if mu is not None else self.openskill_system.DEFAULT_MU
            actual_sigma = sigma if sigma is not None else self.openskill_system.DEFAULT_SIGMA
            team2_ordinals.append(self.openskill_system.ordinal(actual_mu, actual_sigma))

        # Calculate win probability using ordinals
        # Higher ordinal = higher skill
        team1_avg_ordinal = sum(team1_ordinals) / len(team1_ordinals) if team1_ordinals else 0
        team2_avg_ordinal = sum(team2_ordinals) / len(team2_ordinals) if team2_ordinals else 0

        # Use a logistic function to convert ordinal difference to win probability
        # Similar to Elo expected score calculation
        ordinal_diff = team1_avg_ordinal - team2_avg_ordinal
        # Scale factor: typical ordinal range is roughly -10 to +15, so 10 point diff ≈ 76% win
        team1_win_prob = 1.0 / (1.0 + 10 ** (-ordinal_diff / 10.0))

        return {
            "team1_win_prob": team1_win_prob,
            "team1_avg_ordinal": team1_avg_ordinal,
            "team2_avg_ordinal": team2_avg_ordinal,
        }
