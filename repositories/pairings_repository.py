"""
Repository for managing pairwise player statistics.
"""

from repositories.base_repository import BaseRepository
from repositories.interfaces import IPairingsRepository


class PairingsRepository(BaseRepository, IPairingsRepository):
    """
    Handles CRUD operations for player pairings statistics.

    Stores pairings canonically with player1_id < player2_id to avoid duplicates.
    """

    def _canonical_pair(self, id1: int, id2: int) -> tuple:
        """Return IDs in canonical order (smaller first)."""
        return (id1, id2) if id1 < id2 else (id2, id1)

    def update_pairings_for_match(
        self,
        match_id: int,
        guild_id: int,
        team1_ids: list[int],
        team2_ids: list[int],
        winning_team: int,
    ) -> None:
        """
        Update pairwise statistics for all player pairs in a match.

        Args:
            match_id: The match ID
            guild_id: Guild ID for multi-server isolation
            team1_ids: List of discord IDs for team 1
            team2_ids: List of discord IDs for team 2
            winning_team: 1 or 2 indicating which team won
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Process teammates on team 1
            team1_won = winning_team == 1
            for i, p1 in enumerate(team1_ids):
                for p2 in team1_ids[i + 1 :]:
                    self._update_together(cursor, guild_id, p1, p2, team1_won, match_id)

            # Process teammates on team 2
            team2_won = winning_team == 2
            for i, p1 in enumerate(team2_ids):
                for p2 in team2_ids[i + 1 :]:
                    self._update_together(cursor, guild_id, p1, p2, team2_won, match_id)

            # Process opponents (team1 vs team2)
            for p1 in team1_ids:
                for p2 in team2_ids:
                    self._update_against(cursor, guild_id, p1, p2, team1_won, match_id)

    def _update_together(self, cursor, guild_id: int, id1: int, id2: int, won: bool, match_id: int) -> None:
        """Update stats for two players who were on the same team."""
        p1, p2 = self._canonical_pair(id1, id2)
        cursor.execute(
            """
            INSERT INTO player_pairings (guild_id, player1_id, player2_id, games_together, wins_together, last_match_id)
            VALUES (?, ?, ?, 1, ?, ?)
            ON CONFLICT(guild_id, player1_id, player2_id) DO UPDATE SET
                games_together = games_together + 1,
                wins_together = wins_together + ?,
                last_match_id = ?,
                updated_at = CURRENT_TIMESTAMP
            """,
            (guild_id, p1, p2, 1 if won else 0, match_id, 1 if won else 0, match_id),
        )

    def _update_against(self, cursor, guild_id: int, id1: int, id2: int, id1_won: bool, match_id: int) -> None:
        """Update stats for two players who were on opposing teams."""
        p1, p2 = self._canonical_pair(id1, id2)
        # If canonical order matches input order, player1_wins_against tracks id1's wins
        # Otherwise, we track id2's wins (which is !id1_won)
        player1_won = id1_won if id1 == p1 else not id1_won

        cursor.execute(
            """
            INSERT INTO player_pairings (guild_id, player1_id, player2_id, games_against, player1_wins_against, last_match_id)
            VALUES (?, ?, ?, 1, ?, ?)
            ON CONFLICT(guild_id, player1_id, player2_id) DO UPDATE SET
                games_against = games_against + 1,
                player1_wins_against = player1_wins_against + ?,
                last_match_id = ?,
                updated_at = CURRENT_TIMESTAMP
            """,
            (guild_id, p1, p2, 1 if player1_won else 0, match_id, 1 if player1_won else 0, match_id),
        )

    def get_pairings_for_player(self, discord_id: int, guild_id: int) -> list[dict]:
        """Get all pairwise stats involving a player in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    player1_id, player2_id,
                    games_together, wins_together,
                    games_against, player1_wins_against,
                    last_match_id
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                """,
                (guild_id, discord_id, discord_id),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_best_teammates(self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with highest win rate when on same team (win rate > 50%)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as teammate_id,
                    games_together,
                    wins_together,
                    CAST(wins_together AS REAL) / games_together as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_together >= ?
                    AND CAST(wins_together AS REAL) / games_together > 0.5
                ORDER BY win_rate DESC, games_together DESC
                LIMIT ?
                """,
                (discord_id, guild_id, discord_id, discord_id, min_games, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_worst_teammates(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get players with lowest win rate when on same team (win rate < 50%)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as teammate_id,
                    games_together,
                    wins_together,
                    CAST(wins_together AS REAL) / games_together as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_together >= ?
                    AND CAST(wins_together AS REAL) / games_together < 0.5
                ORDER BY win_rate ASC, games_together DESC
                LIMIT ?
                """,
                (discord_id, guild_id, discord_id, discord_id, min_games, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_best_matchups(self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with highest win rate when on opposing teams (win rate > 50%)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as opponent_id,
                    games_against,
                    CASE WHEN player1_id = ?
                        THEN player1_wins_against
                        ELSE games_against - player1_wins_against
                    END as wins_against,
                    CAST(
                        CASE WHEN player1_id = ?
                            THEN player1_wins_against
                            ELSE games_against - player1_wins_against
                        END AS REAL
                    ) / games_against as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_against >= ?
                    AND CAST(
                        CASE WHEN player1_id = ?
                            THEN player1_wins_against
                            ELSE games_against - player1_wins_against
                        END AS REAL
                    ) / games_against > 0.5
                ORDER BY win_rate DESC, games_against DESC
                LIMIT ?
                """,
                (
                    discord_id,
                    discord_id,
                    discord_id,
                    guild_id,
                    discord_id,
                    discord_id,
                    min_games,
                    discord_id,
                    limit,
                ),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_worst_matchups(self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with lowest win rate when on opposing teams (win rate < 50%)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as opponent_id,
                    games_against,
                    CASE WHEN player1_id = ?
                        THEN player1_wins_against
                        ELSE games_against - player1_wins_against
                    END as wins_against,
                    CAST(
                        CASE WHEN player1_id = ?
                            THEN player1_wins_against
                            ELSE games_against - player1_wins_against
                        END AS REAL
                    ) / games_against as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_against >= ?
                    AND CAST(
                        CASE WHEN player1_id = ?
                            THEN player1_wins_against
                            ELSE games_against - player1_wins_against
                        END AS REAL
                    ) / games_against < 0.5
                ORDER BY win_rate ASC, games_against DESC
                LIMIT ?
                """,
                (
                    discord_id,
                    discord_id,
                    discord_id,
                    guild_id,
                    discord_id,
                    discord_id,
                    min_games,
                    discord_id,
                    limit,
                ),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_most_played_with(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get teammates sorted by most games played together."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as teammate_id,
                    games_together,
                    wins_together,
                    CAST(wins_together AS REAL) / games_together as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_together >= ?
                ORDER BY games_together DESC, win_rate DESC
                LIMIT ?
                """,
                (discord_id, guild_id, discord_id, discord_id, min_games, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_most_played_against(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents sorted by most games played against."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as opponent_id,
                    games_against,
                    CASE WHEN player1_id = ?
                        THEN player1_wins_against
                        ELSE games_against - player1_wins_against
                    END as wins_against,
                    CAST(
                        CASE WHEN player1_id = ?
                            THEN player1_wins_against
                            ELSE games_against - player1_wins_against
                        END AS REAL
                    ) / games_against as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_against >= ?
                ORDER BY games_against DESC, win_rate DESC
                LIMIT ?
                """,
                (discord_id, discord_id, discord_id, guild_id, discord_id, discord_id, min_games, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_evenly_matched_teammates(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get teammates with exactly 50% win rate."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as teammate_id,
                    games_together,
                    wins_together,
                    CAST(wins_together AS REAL) / games_together as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_together >= ?
                    AND CAST(wins_together AS REAL) / games_together = 0.5
                ORDER BY games_together DESC
                LIMIT ?
                """,
                (discord_id, guild_id, discord_id, discord_id, min_games, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_evenly_matched_opponents(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents with exactly 50% win rate."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    CASE WHEN player1_id = ? THEN player2_id ELSE player1_id END as opponent_id,
                    games_against,
                    CASE WHEN player1_id = ?
                        THEN player1_wins_against
                        ELSE games_against - player1_wins_against
                    END as wins_against,
                    CAST(
                        CASE WHEN player1_id = ?
                            THEN player1_wins_against
                            ELSE games_against - player1_wins_against
                        END AS REAL
                    ) / games_against as win_rate
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                    AND games_against >= ?
                    AND CAST(
                        CASE WHEN player1_id = ?
                            THEN player1_wins_against
                            ELSE games_against - player1_wins_against
                        END AS REAL
                    ) / games_against = 0.5
                ORDER BY games_against DESC
                LIMIT ?
                """,
                (
                    discord_id,
                    discord_id,
                    discord_id,
                    guild_id,
                    discord_id,
                    discord_id,
                    min_games,
                    discord_id,
                    limit,
                ),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_pairing_counts(self, discord_id: int, guild_id: int, min_games: int = 1) -> dict:
        """Get total counts of unique teammates and opponents."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    COUNT(CASE WHEN games_together >= ? THEN 1 END) as unique_teammates,
                    COUNT(CASE WHEN games_against >= ? THEN 1 END) as unique_opponents
                FROM player_pairings
                WHERE guild_id = ? AND (player1_id = ? OR player2_id = ?)
                """,
                (min_games, min_games, guild_id, discord_id, discord_id),
            )
            row = cursor.fetchone()
            return {
                "unique_teammates": row["unique_teammates"] or 0,
                "unique_opponents": row["unique_opponents"] or 0,
            }

    def get_head_to_head(self, player1_id: int, player2_id: int, guild_id: int) -> dict | None:
        """Get detailed stats between two specific players in a guild."""
        p1, p2 = self._canonical_pair(player1_id, player2_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    player1_id, player2_id,
                    games_together, wins_together,
                    games_against, player1_wins_against,
                    last_match_id
                FROM player_pairings
                WHERE guild_id = ? AND player1_id = ? AND player2_id = ?
                """,
                (guild_id, p1, p2),
            )
            row = cursor.fetchone()
            if not row:
                return None

            result = dict(row)
            # Add perspective-adjusted stats for the queried player
            if player1_id == p1:
                result["queried_player_wins_against"] = result["player1_wins_against"]
            else:
                result["queried_player_wins_against"] = (
                    result["games_against"] - result["player1_wins_against"]
                )
            return result

    def reverse_pairings_for_match(
        self,
        guild_id: int,
        team1_ids: list[int],
        team2_ids: list[int],
        original_winning_team: int,
    ) -> None:
        """
        Reverse pairings that were incremented during original match recording.

        This decrements the stats that were added when the match was first recorded.
        Used during match correction to undo the original recording's effects.

        Args:
            guild_id: Guild ID
            team1_ids: List of discord IDs for team 1 (Radiant)
            team2_ids: List of discord IDs for team 2 (Dire)
            original_winning_team: 1 or 2 indicating which team originally won
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Reverse teammates on team 1
            team1_won = original_winning_team == 1
            for i, p1 in enumerate(team1_ids):
                for p2 in team1_ids[i + 1:]:
                    self._reverse_together(cursor, guild_id, p1, p2, team1_won)

            # Reverse teammates on team 2
            team2_won = original_winning_team == 2
            for i, p1 in enumerate(team2_ids):
                for p2 in team2_ids[i + 1:]:
                    self._reverse_together(cursor, guild_id, p1, p2, team2_won)

            # Reverse opponents (team1 vs team2)
            for p1 in team1_ids:
                for p2 in team2_ids:
                    self._reverse_against(cursor, guild_id, p1, p2, team1_won)

    def _reverse_together(self, cursor, guild_id: int, id1: int, id2: int, won: bool) -> None:
        """Reverse stats for two players who were on the same team."""
        p1, p2 = self._canonical_pair(id1, id2)
        cursor.execute(
            """
            UPDATE player_pairings
            SET games_together = games_together - 1,
                wins_together = wins_together - ?,
                updated_at = CURRENT_TIMESTAMP
            WHERE guild_id = ? AND player1_id = ? AND player2_id = ?
            """,
            (1 if won else 0, guild_id, p1, p2),
        )

    def _reverse_against(self, cursor, guild_id: int, id1: int, id2: int, id1_won: bool) -> None:
        """Reverse stats for two players who were on opposing teams."""
        p1, p2 = self._canonical_pair(id1, id2)
        player1_won = id1_won if id1 == p1 else not id1_won

        cursor.execute(
            """
            UPDATE player_pairings
            SET games_against = games_against - 1,
                player1_wins_against = player1_wins_against - ?,
                updated_at = CURRENT_TIMESTAMP
            WHERE guild_id = ? AND player1_id = ? AND player2_id = ?
            """,
            (1 if player1_won else 0, guild_id, p1, p2),
        )

    def rebuild_all_pairings(self, guild_id: int) -> int:
        """
        Recalculate all pairings from match history for a guild.

        Returns count of pairings updated.
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Clear existing pairings for this guild
            cursor.execute("DELETE FROM player_pairings WHERE guild_id = ?", (guild_id,))

            # Get all matches with participants for this guild
            cursor.execute(
                """
                SELECT m.match_id, m.winning_team, mp.discord_id, mp.team_number
                FROM matches m
                JOIN match_participants mp ON m.match_id = mp.match_id
                WHERE m.guild_id = ? AND m.winning_team IS NOT NULL
                ORDER BY m.match_id
                """,
                (guild_id,),
            )
            rows = cursor.fetchall()

            # Group by match
            matches: dict[int, dict] = {}
            for row in rows:
                match_id = row["match_id"]
                if match_id not in matches:
                    matches[match_id] = {
                        "winning_team": row["winning_team"],
                        "team1": [],
                        "team2": [],
                    }
                if row["team_number"] == 1:
                    matches[match_id]["team1"].append(row["discord_id"])
                else:
                    matches[match_id]["team2"].append(row["discord_id"])

            # Process each match
            for match_id, data in matches.items():
                team1_ids = data["team1"]
                team2_ids = data["team2"]
                winning_team = data["winning_team"]

                # Process teammates on team 1
                team1_won = winning_team == 1
                for i, p1 in enumerate(team1_ids):
                    for p2 in team1_ids[i + 1 :]:
                        self._update_together(cursor, guild_id, p1, p2, team1_won, match_id)

                # Process teammates on team 2
                team2_won = winning_team == 2
                for i, p1 in enumerate(team2_ids):
                    for p2 in team2_ids[i + 1 :]:
                        self._update_together(cursor, guild_id, p1, p2, team2_won, match_id)

                # Process opponents
                for p1 in team1_ids:
                    for p2 in team2_ids:
                        self._update_against(cursor, guild_id, p1, p2, team1_won, match_id)

            # Count total pairings for this guild
            cursor.execute("SELECT COUNT(*) as count FROM player_pairings WHERE guild_id = ?", (guild_id,))
            return cursor.fetchone()["count"]
