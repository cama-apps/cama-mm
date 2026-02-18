"""
Repository for match data access.
"""

import json
import logging

from repositories.base_repository import BaseRepository
from repositories.interfaces import IMatchRepository

logger = logging.getLogger("cama_bot.repositories.match")


class MatchRepository(BaseRepository, IMatchRepository):
    """
    Handles all match-related database operations.

    Responsibilities:
    - Match recording
    - Match participant tracking
    - Rating history
    """

    def record_match(
        self,
        team1_ids: list[int],
        team2_ids: list[int],
        winning_team: int,
        guild_id: int,
        radiant_team_ids: list[int] | None = None,
        dire_team_ids: list[int] | None = None,
        dotabuff_match_id: str | None = None,
        notes: str | None = None,
        lobby_type: str = "shuffle",
        balancing_rating_system: str = "glicko",
    ) -> int:
        """
        Record a match result.

        Convention: team1 = Radiant, team2 = Dire.
        winning_team: 1 = Radiant won, 2 = Dire won.

        Args:
            team1_ids: Discord IDs of Radiant players
            team2_ids: Discord IDs of Dire players
            winning_team: 1 (Radiant won) or 2 (Dire won)
            guild_id: Guild ID for multi-server isolation
            radiant_team_ids: Deprecated; ignored (team1 is Radiant)
            dire_team_ids: Deprecated; ignored (team2 is Dire)
            dotabuff_match_id: Optional external match ID
            notes: Optional match notes
            lobby_type: 'shuffle' or 'draft'
            balancing_rating_system: 'glicko' or 'openskill' (for experiment tracking)

        Returns:
            Match ID
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Insert match record (team1=Radiant, team2=Dire)
            cursor.execute(
                """
                INSERT INTO matches (guild_id, team1_players, team2_players, winning_team,
                                    dotabuff_match_id, notes, lobby_type, balancing_rating_system)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """,
                (
                    guild_id,
                    json.dumps(team1_ids),
                    json.dumps(team2_ids),
                    winning_team,
                    dotabuff_match_id,
                    notes,
                    lobby_type,
                    balancing_rating_system,
                ),
            )

            match_id = cursor.lastrowid

            # Insert participants with side (team1=radiant, team2=dire)
            team1_won = winning_team == 1

            for player_id in team1_ids:
                cursor.execute(
                    """
                    INSERT INTO match_participants (match_id, discord_id, guild_id, team_number, won, side)
                    VALUES (?, ?, ?, 1, ?, ?)
                """,
                    (match_id, player_id, guild_id, team1_won, "radiant"),
                )

            for player_id in team2_ids:
                cursor.execute(
                    """
                    INSERT INTO match_participants (match_id, discord_id, guild_id, team_number, won, side)
                    VALUES (?, ?, ?, 2, ?, ?)
                """,
                    (match_id, player_id, guild_id, not team1_won, "dire"),
                )

            return match_id

    def add_rating_history(
        self,
        discord_id: int,
        guild_id: int,
        rating: float,
        match_id: int | None = None,
        rating_before: float | None = None,
        rd_before: float | None = None,
        rd_after: float | None = None,
        volatility_before: float | None = None,
        volatility_after: float | None = None,
        expected_team_win_prob: float | None = None,
        team_number: int | None = None,
        won: bool | None = None,
        os_mu_before: float | None = None,
        os_mu_after: float | None = None,
        os_sigma_before: float | None = None,
        os_sigma_after: float | None = None,
        fantasy_weight: float | None = None,
        streak_length: int | None = None,
        streak_multiplier: float | None = None,
    ) -> None:
        """Record a rating change in history (Glicko-2 and optionally OpenSkill)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO rating_history (
                    discord_id,
                    guild_id,
                    rating,
                    rating_before,
                    rd_before,
                    rd_after,
                    volatility_before,
                    volatility_after,
                    expected_team_win_prob,
                    team_number,
                    won,
                    match_id,
                    os_mu_before,
                    os_mu_after,
                    os_sigma_before,
                    os_sigma_after,
                    fantasy_weight,
                    streak_length,
                    streak_multiplier
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
                (
                    discord_id,
                    guild_id,
                    rating,
                    rating_before,
                    rd_before,
                    rd_after,
                    volatility_before,
                    volatility_after,
                    expected_team_win_prob,
                    team_number,
                    won,
                    match_id,
                    os_mu_before,
                    os_mu_after,
                    os_sigma_before,
                    os_sigma_after,
                    fantasy_weight,
                    streak_length,
                    streak_multiplier,
                ),
            )

    def save_pending_match(self, guild_id: int | None, payload: dict) -> int:
        """
        Save a pending match and return its ID.

        Unlike before, this always creates a NEW pending match (no upsert).
        This allows multiple concurrent pending matches per guild.

        Returns:
            pending_match_id: The auto-generated ID for this pending match
        """
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO pending_matches (guild_id, payload)
                VALUES (?, ?)
                """,
                (normalized, json.dumps(payload)),
            )
            return cursor.lastrowid

    def update_pending_match(self, pending_match_id: int, payload: dict) -> None:
        """
        Update an existing pending match's payload.

        Args:
            pending_match_id: The ID of the pending match to update
            payload: The updated payload dictionary
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE pending_matches
                SET payload = ?, updated_at = CURRENT_TIMESTAMP
                WHERE pending_match_id = ?
                """,
                (json.dumps(payload), pending_match_id),
            )

    def get_pending_match(self, guild_id: int | None) -> dict | None:
        """
        Get a pending match for backward compatibility.

        Returns:
            - The single pending match if exactly one exists
            - None if no pending matches or multiple exist (forces explicit handling)
        """
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT pending_match_id, payload FROM pending_matches WHERE guild_id = ?",
                (normalized,),
            )
            rows = cursor.fetchall()
            if len(rows) == 1:
                payload = json.loads(rows[0]["payload"])
                payload["pending_match_id"] = rows[0]["pending_match_id"]
                return payload
            return None

    def get_pending_matches(self, guild_id: int | None) -> list[dict]:
        """
        Get all pending matches for a guild.

        Returns:
            List of pending match payloads with pending_match_id included
        """
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT pending_match_id, payload, created_at
                FROM pending_matches
                WHERE guild_id = ?
                ORDER BY created_at ASC
                """,
                (normalized,),
            )
            results = []
            for row in cursor.fetchall():
                payload = json.loads(row["payload"])
                payload["pending_match_id"] = row["pending_match_id"]
                payload["created_at"] = row["created_at"]
                results.append(payload)
            return results

    def get_pending_match_by_id(self, pending_match_id: int) -> dict | None:
        """
        Get a specific pending match by its ID.

        Args:
            pending_match_id: The pending match ID

        Returns:
            The pending match payload with pending_match_id included, or None
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT pending_match_id, guild_id, payload FROM pending_matches WHERE pending_match_id = ?",
                (pending_match_id,),
            )
            row = cursor.fetchone()
            if not row:
                return None
            payload = json.loads(row["payload"])
            payload["pending_match_id"] = row["pending_match_id"]
            payload["guild_id"] = row["guild_id"]
            return payload

    def get_pending_match_for_player(self, guild_id: int | None, discord_id: int) -> dict | None:
        """
        Auto-detect which pending match a player is in.

        Args:
            guild_id: The guild ID
            discord_id: The player's Discord ID

        Returns:
            The pending match payload if player is a participant, None otherwise
        """
        pending_matches = self.get_pending_matches(guild_id)
        for match in pending_matches:
            radiant_ids = set(match.get("radiant_team_ids") or [])
            dire_ids = set(match.get("dire_team_ids") or [])
            if discord_id in radiant_ids or discord_id in dire_ids:
                return match
        return None

    def get_all_pending_match_player_ids(self, guild_id: int | None) -> set[int]:
        """
        Get all player IDs currently in any pending match for a guild.

        Returns:
            Set of Discord IDs of all players in pending matches
        """
        pending_matches = self.get_pending_matches(guild_id)
        player_ids = set()
        for match in pending_matches:
            player_ids.update(match.get("radiant_team_ids") or [])
            player_ids.update(match.get("dire_team_ids") or [])
        return player_ids

    def clear_pending_match(self, guild_id: int | None, pending_match_id: int | None = None) -> None:
        """
        Clear pending match(es).

        Args:
            guild_id: The guild ID
            pending_match_id: If provided, clear only this specific match.
                             If None, clear ALL pending matches for the guild.
        """
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            if pending_match_id is not None:
                cursor.execute(
                    "DELETE FROM pending_matches WHERE pending_match_id = ? AND guild_id = ?",
                    (pending_match_id, normalized),
                )
            else:
                cursor.execute("DELETE FROM pending_matches WHERE guild_id = ?", (normalized,))

    def consume_pending_match(self, guild_id: int | None, pending_match_id: int | None = None) -> dict | None:
        """
        Get and delete a pending match atomically.

        Args:
            guild_id: The guild ID
            pending_match_id: If provided, consume this specific match.
                             If None and only one match exists, consume it.

        Returns:
            The pending match payload or None
        """
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None:
                cursor.execute(
                    "SELECT pending_match_id, payload FROM pending_matches WHERE pending_match_id = ? AND guild_id = ?",
                    (pending_match_id, normalized),
                )
            else:
                # Only consume if exactly one match exists
                cursor.execute(
                    "SELECT pending_match_id, payload FROM pending_matches WHERE guild_id = ?",
                    (normalized,),
                )

            rows = cursor.fetchall()
            if not rows:
                return None

            # If no specific ID and multiple matches, don't consume
            if pending_match_id is None and len(rows) > 1:
                return None

            row = rows[0]
            match_id = row["pending_match_id"]
            cursor.execute("DELETE FROM pending_matches WHERE pending_match_id = ?", (match_id,))
            payload = json.loads(row["payload"])
            payload["pending_match_id"] = match_id
            return payload

    def get_match(self, match_id: int, guild_id: int | None = None) -> dict | None:
        """Get match by ID, optionally filtered by guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            if guild_id is not None:
                cursor.execute(
                    "SELECT * FROM matches WHERE match_id = ? AND guild_id = ?",
                    (match_id, guild_id),
                )
            else:
                cursor.execute("SELECT * FROM matches WHERE match_id = ?", (match_id,))
            row = cursor.fetchone()

            if not row:
                return None

            return {
                "match_id": row["match_id"],
                "team1_players": json.loads(row["team1_players"]),
                "team2_players": json.loads(row["team2_players"]),
                "winning_team": row["winning_team"],
                "match_date": row["match_date"],
                "dotabuff_match_id": row["dotabuff_match_id"],
                "notes": row["notes"],
                # Enrichment fields (may be None if not enriched)
                "valve_match_id": row["valve_match_id"] if "valve_match_id" in row.keys() else None,
                "duration_seconds": row["duration_seconds"]
                if "duration_seconds" in row.keys()
                else None,
                "radiant_score": row["radiant_score"] if "radiant_score" in row.keys() else None,
                "dire_score": row["dire_score"] if "dire_score" in row.keys() else None,
                "game_mode": row["game_mode"] if "game_mode" in row.keys() else None,
                "lobby_type": row["lobby_type"] if "lobby_type" in row.keys() else "shuffle",
                "balancing_rating_system": row["balancing_rating_system"]
                if "balancing_rating_system" in row.keys()
                else "glicko",
            }

    def get_player_matches(self, discord_id: int, guild_id: int, limit: int = 10) -> list[dict]:
        """Get recent matches for a player in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT m.*, mp.team_number, mp.won, mp.side
                FROM matches m
                JOIN match_participants mp ON m.match_id = mp.match_id
                WHERE mp.discord_id = ? AND mp.guild_id = ?
                ORDER BY m.match_date DESC
                LIMIT ?
            """,
                (discord_id, guild_id, limit),
            )

            rows = cursor.fetchall()
            return [
                {
                    "match_id": row["match_id"],
                    "team1_players": json.loads(row["team1_players"]),
                    "team2_players": json.loads(row["team2_players"]),
                    "winning_team": row["winning_team"],
                    "match_date": row["match_date"],
                    "player_team": row["team_number"],
                    "player_won": bool(row["won"]),
                    "side": row["side"],
                    "valve_match_id": row["valve_match_id"],
                    "lobby_type": row["lobby_type"] if "lobby_type" in row.keys() else "shuffle",
                }
                for row in rows
            ]

    def get_rating_history(self, discord_id: int, guild_id: int, limit: int = 20) -> list[dict]:
        """Get rating history for a player in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM rating_history
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
            """,
                (discord_id, guild_id, limit),
            )

            rows = cursor.fetchall()
            return [
                {
                    "rating": row["rating"],
                    "match_id": row["match_id"],
                    "timestamp": row["timestamp"],
                    "streak_length": row["streak_length"] if "streak_length" in row.keys() else None,
                    "streak_multiplier": row["streak_multiplier"] if "streak_multiplier" in row.keys() else None,
                }
                for row in rows
            ]

    def get_player_recent_outcomes(self, discord_id: int, guild_id: int, limit: int = 20) -> list[bool]:
        """
        Get recent match outcomes for a player in a guild.

        Returns a list of booleans (True=win, False=loss) in reverse chronological
        order (most recent first). Uses the `won` column from rating_history.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            limit: Maximum number of outcomes to return

        Returns:
            List of booleans representing win/loss outcomes
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT won FROM rating_history
                WHERE discord_id = ? AND guild_id = ? AND won IS NOT NULL
                ORDER BY id DESC
                LIMIT ?
            """,
                (discord_id, guild_id, limit),
            )
            rows = cursor.fetchall()
            return [bool(row["won"]) for row in rows]

    def get_player_rating_history_detailed(self, discord_id: int, guild_id: int, limit: int = 50) -> list[dict]:
        """Get detailed rating history for a player in a guild including prediction and OpenSkill data."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT rh.*, m.lobby_type
                FROM rating_history rh
                LEFT JOIN matches m ON rh.match_id = m.match_id
                WHERE rh.discord_id = ? AND rh.guild_id = ?
                ORDER BY rh.timestamp DESC
                LIMIT ?
            """,
                (discord_id, guild_id, limit),
            )
            rows = cursor.fetchall()
            return [
                {
                    "rating": row["rating"],
                    "rating_before": row["rating_before"],
                    "rd_before": row["rd_before"],
                    "rd_after": row["rd_after"],
                    "volatility_before": row["volatility_before"],
                    "volatility_after": row["volatility_after"],
                    "expected_team_win_prob": row["expected_team_win_prob"],
                    "team_number": row["team_number"],
                    "won": row["won"],
                    "match_id": row["match_id"],
                    "timestamp": row["timestamp"],
                    "lobby_type": row["lobby_type"] if "lobby_type" in row.keys() else "shuffle",
                    "os_mu_before": row["os_mu_before"] if "os_mu_before" in row.keys() else None,
                    "os_mu_after": row["os_mu_after"] if "os_mu_after" in row.keys() else None,
                    "os_sigma_before": row["os_sigma_before"] if "os_sigma_before" in row.keys() else None,
                    "os_sigma_after": row["os_sigma_after"] if "os_sigma_after" in row.keys() else None,
                }
                for row in rows
            ]

    def get_rating_history_for_match(self, match_id: int) -> list[dict]:
        """
        Get all rating history entries for a specific match.

        Used for picking notable winners for flavor text.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, rating, rating_before, expected_team_win_prob, won
                FROM rating_history
                WHERE match_id = ?
            """,
                (match_id,),
            )
            rows = cursor.fetchall()
            return [
                {
                    "discord_id": row["discord_id"],
                    "rating": row["rating"],
                    "rating_before": row["rating_before"],
                    "expected_team_win_prob": row["expected_team_win_prob"],
                    "won": row["won"],
                }
                for row in rows
            ]

    def get_os_ratings_for_match(self, match_id: int) -> dict:
        """
        Get OpenSkill ratings (before match) for all players in a match, grouped by team.

        Returns:
            Dict with 'team1' and 'team2' keys, each containing list of (os_mu, os_sigma) tuples.
            Returns empty lists if no OpenSkill data available.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, team_number, os_mu_before, os_sigma_before
                FROM rating_history
                WHERE match_id = ?
                  AND os_mu_before IS NOT NULL
                  AND os_sigma_before IS NOT NULL
            """,
                (match_id,),
            )
            rows = cursor.fetchall()

            team1_ratings: list[tuple[float, float]] = []
            team2_ratings: list[tuple[float, float]] = []

            for row in rows:
                rating_tuple = (row["os_mu_before"], row["os_sigma_before"])
                if row["team_number"] == 1:
                    team1_ratings.append(rating_tuple)
                elif row["team_number"] == 2:
                    team2_ratings.append(rating_tuple)

            return {"team1": team1_ratings, "team2": team2_ratings}

    def get_recent_rating_history(self, guild_id: int, limit: int = 200) -> list[dict]:
        """Get recent rating history entries for all players in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT *
                FROM rating_history
                WHERE guild_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
            """,
                (guild_id, limit),
            )
            rows = cursor.fetchall()
            return [
                {
                    "discord_id": row["discord_id"],
                    "rating": row["rating"],
                    "rating_before": row["rating_before"],
                    "rd_before": row["rd_before"],
                    "rd_after": row["rd_after"],
                    "volatility_before": row["volatility_before"],
                    "volatility_after": row["volatility_after"],
                    "expected_team_win_prob": row["expected_team_win_prob"],
                    "team_number": row["team_number"],
                    "won": row["won"],
                    "match_id": row["match_id"],
                    "timestamp": row["timestamp"],
                }
                for row in rows
            ]

    def delete_all_matches(self, guild_id: int) -> int:
        """
        Delete all matches in a guild (for testing).

        Returns:
            Number of matches deleted
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute("SELECT COUNT(*) FROM matches WHERE guild_id = ?", (guild_id,))
            count = cursor.fetchone()[0]

            # Get match IDs for this guild to clean up predictions
            cursor.execute("SELECT match_id FROM matches WHERE guild_id = ?", (guild_id,))
            match_ids = [row["match_id"] for row in cursor.fetchall()]
            if match_ids:
                placeholders = ",".join("?" * len(match_ids))
                cursor.execute(f"DELETE FROM match_predictions WHERE match_id IN ({placeholders})", match_ids)

            cursor.execute("DELETE FROM matches WHERE guild_id = ?", (guild_id,))
            cursor.execute("DELETE FROM match_participants WHERE guild_id = ?", (guild_id,))
            cursor.execute("DELETE FROM rating_history WHERE guild_id = ?", (guild_id,))

            return count

    def get_match_count(self, guild_id: int) -> int:
        """Get total match count for a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM matches WHERE guild_id = ?", (guild_id,))
            row = cursor.fetchone()
            return row["count"] if row else 0

    def add_match_prediction(
        self,
        match_id: int,
        radiant_rating: float,
        dire_rating: float,
        radiant_rd: float,
        dire_rd: float,
        expected_radiant_win_prob: float,
    ) -> None:
        """Store pre-match expected win probability and team stats."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO match_predictions (
                    match_id,
                    radiant_rating,
                    dire_rating,
                    radiant_rd,
                    dire_rd,
                    expected_radiant_win_prob
                )
                VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(match_id) DO UPDATE SET
                    radiant_rating = excluded.radiant_rating,
                    dire_rating = excluded.dire_rating,
                    radiant_rd = excluded.radiant_rd,
                    dire_rd = excluded.dire_rd,
                    expected_radiant_win_prob = excluded.expected_radiant_win_prob,
                    timestamp = CURRENT_TIMESTAMP
            """,
                (
                    match_id,
                    radiant_rating,
                    dire_rating,
                    radiant_rd,
                    dire_rd,
                    expected_radiant_win_prob,
                ),
            )

    def get_recent_match_predictions(self, guild_id: int | None, limit: int = 200) -> list[dict]:
        """Get recent match predictions with outcomes for a guild."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    mp.match_id,
                    mp.expected_radiant_win_prob,
                    mp.radiant_rating,
                    mp.dire_rating,
                    mp.radiant_rd,
                    mp.dire_rd,
                    m.winning_team,
                    m.match_date
                FROM match_predictions mp
                JOIN matches m ON m.match_id = mp.match_id
                WHERE m.guild_id = ?
                ORDER BY m.match_date DESC
                LIMIT ?
            """,
                (normalized_guild_id, limit),
            )
            rows = cursor.fetchall()
            return [
                {
                    "match_id": row["match_id"],
                    "expected_radiant_win_prob": row["expected_radiant_win_prob"],
                    "radiant_rating": row["radiant_rating"],
                    "dire_rating": row["dire_rating"],
                    "radiant_rd": row["radiant_rd"],
                    "dire_rd": row["dire_rd"],
                    "winning_team": row["winning_team"],
                    "match_date": row["match_date"],
                }
                for row in rows
            ]

    def get_most_recent_match(self, guild_id: int) -> dict | None:
        """Get the most recently recorded match in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM matches WHERE guild_id = ? ORDER BY match_date DESC, match_id DESC LIMIT 1",
                (guild_id,),
            )
            row = cursor.fetchone()

            if not row:
                return None

            return {
                "match_id": row["match_id"],
                "team1_players": json.loads(row["team1_players"]),
                "team2_players": json.loads(row["team2_players"]),
                "winning_team": row["winning_team"],
                "match_date": row["match_date"],
                "dotabuff_match_id": row["dotabuff_match_id"],
                "valve_match_id": row["valve_match_id"] if "valve_match_id" in row.keys() else None,
                "notes": row["notes"],
            }

    def get_last_match_participant_ids(self, guild_id: int) -> set[int]:
        """Get Discord IDs of participants from the most recently recorded match in a guild."""
        match = self.get_most_recent_match(guild_id)
        if not match:
            return set()
        team1_ids = match.get("team1_players", [])
        team2_ids = match.get("team2_players", [])
        return set(team1_ids + team2_ids)

    def get_matches_without_enrichment(self, guild_id: int, limit: int = 10) -> list[dict]:
        """Get matches that don't have Valve enrichment data yet in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM matches
                WHERE guild_id = ? AND valve_match_id IS NULL
                ORDER BY match_date DESC
                LIMIT ?
                """,
                (guild_id, limit),
            )
            rows = cursor.fetchall()
            return [
                {
                    "match_id": row["match_id"],
                    "team1_players": json.loads(row["team1_players"]),
                    "team2_players": json.loads(row["team2_players"]),
                    "winning_team": row["winning_team"],
                    "match_date": row["match_date"],
                }
                for row in rows
            ]

    def set_valve_match_id(self, match_id: int, valve_match_id: int) -> None:
        """Set the Valve match ID for an internal match."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE matches SET valve_match_id = ? WHERE match_id = ?",
                (valve_match_id, match_id),
            )

    def update_match_enrichment(
        self,
        match_id: int,
        valve_match_id: int,
        duration_seconds: int,
        radiant_score: int,
        dire_score: int,
        game_mode: int,
        enrichment_data: str | None = None,
        enrichment_source: str | None = None,
        enrichment_confidence: float | None = None,
    ) -> None:
        """Update match with API enrichment data."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE matches
                SET valve_match_id = ?,
                    duration_seconds = ?,
                    radiant_score = ?,
                    dire_score = ?,
                    game_mode = ?,
                    enrichment_data = ?,
                    enrichment_source = ?,
                    enrichment_confidence = ?
                WHERE match_id = ?
                """,
                (
                    valve_match_id,
                    duration_seconds,
                    radiant_score,
                    dire_score,
                    game_mode,
                    enrichment_data,
                    enrichment_source,
                    enrichment_confidence,
                    match_id,
                ),
            )

    def update_participant_stats(
        self,
        match_id: int,
        discord_id: int,
        hero_id: int,
        kills: int,
        deaths: int,
        assists: int,
        gpm: int,
        xpm: int,
        hero_damage: int,
        tower_damage: int,
        last_hits: int,
        denies: int,
        net_worth: int,
        hero_healing: int = 0,
        lane_role: int | None = None,
        lane_efficiency: int | None = None,
        # Fantasy fields
        towers_killed: int | None = None,
        roshans_killed: int | None = None,
        teamfight_participation: float | None = None,
        obs_placed: int | None = None,
        sen_placed: int | None = None,
        camps_stacked: int | None = None,
        rune_pickups: int | None = None,
        firstblood_claimed: int | None = None,
        stuns: float | None = None,
        fantasy_points: float | None = None,
    ) -> None:
        """Update a match participant with enriched stats from OpenDota API."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE match_participants
                SET hero_id = ?,
                    kills = ?,
                    deaths = ?,
                    assists = ?,
                    gpm = ?,
                    xpm = ?,
                    hero_damage = ?,
                    tower_damage = ?,
                    last_hits = ?,
                    denies = ?,
                    net_worth = ?,
                    hero_healing = ?,
                    lane_role = ?,
                    lane_efficiency = ?,
                    towers_killed = ?,
                    roshans_killed = ?,
                    teamfight_participation = ?,
                    obs_placed = ?,
                    sen_placed = ?,
                    camps_stacked = ?,
                    rune_pickups = ?,
                    firstblood_claimed = ?,
                    stuns = ?,
                    fantasy_points = ?
                WHERE match_id = ? AND discord_id = ?
                """,
                (
                    hero_id,
                    kills,
                    deaths,
                    assists,
                    gpm,
                    xpm,
                    hero_damage,
                    tower_damage,
                    last_hits,
                    denies,
                    net_worth,
                    hero_healing,
                    lane_role,
                    lane_efficiency,
                    towers_killed,
                    roshans_killed,
                    teamfight_participation,
                    obs_placed,
                    sen_placed,
                    camps_stacked,
                    rune_pickups,
                    firstblood_claimed,
                    stuns,
                    fantasy_points,
                    match_id,
                    discord_id,
                ),
            )

    def update_participant_stats_bulk(self, match_id: int, updates: list[dict]) -> int:
        """
        Update all participants in a single transaction.

        Args:
            match_id: The match ID
            updates: List of dicts with keys:
                - discord_id (required)
                - hero_id, kills, deaths, assists, gpm, xpm, hero_damage, tower_damage,
                  last_hits, denies, net_worth, hero_healing, lane_role, lane_efficiency,
                  towers_killed, roshans_killed, teamfight_participation, obs_placed,
                  sen_placed, camps_stacked, rune_pickups, firstblood_claimed, stuns,
                  fantasy_points

        Returns:
            Number of rows updated
        """
        if not updates:
            return 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.executemany(
                """
                UPDATE match_participants
                SET hero_id = ?,
                    kills = ?,
                    deaths = ?,
                    assists = ?,
                    gpm = ?,
                    xpm = ?,
                    hero_damage = ?,
                    tower_damage = ?,
                    last_hits = ?,
                    denies = ?,
                    net_worth = ?,
                    hero_healing = ?,
                    lane_role = ?,
                    lane_efficiency = ?,
                    towers_killed = ?,
                    roshans_killed = ?,
                    teamfight_participation = ?,
                    obs_placed = ?,
                    sen_placed = ?,
                    camps_stacked = ?,
                    rune_pickups = ?,
                    firstblood_claimed = ?,
                    stuns = ?,
                    fantasy_points = ?
                WHERE match_id = ? AND discord_id = ?
                """,
                [
                    (
                        u.get("hero_id"),
                        u.get("kills"),
                        u.get("deaths"),
                        u.get("assists"),
                        u.get("gpm"),
                        u.get("xpm"),
                        u.get("hero_damage"),
                        u.get("tower_damage"),
                        u.get("last_hits"),
                        u.get("denies"),
                        u.get("net_worth"),
                        u.get("hero_healing"),
                        u.get("lane_role"),
                        u.get("lane_efficiency"),
                        u.get("towers_killed"),
                        u.get("roshans_killed"),
                        u.get("teamfight_participation"),
                        u.get("obs_placed"),
                        u.get("sen_placed"),
                        u.get("camps_stacked"),
                        u.get("rune_pickups"),
                        u.get("firstblood_claimed"),
                        u.get("stuns"),
                        u.get("fantasy_points"),
                        match_id,
                        u["discord_id"],
                    )
                    for u in updates
                ],
            )
            return cursor.rowcount

    def get_match_participants(self, match_id: int) -> list[dict]:
        """Get all participants for a match with their stats."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM match_participants
                WHERE match_id = ?
                """,
                (match_id,),
            )
            rows = cursor.fetchall()
            return [dict(row) for row in rows]

    def get_player_hero_stats(self, discord_id: int, guild_id: int) -> dict:
        """
        Get hero statistics for a player from enriched matches in a guild.

        Returns:
            Dict with:
            - last_hero_id: int or None (most recent enriched match)
            - hero_counts: list of (hero_id, games, wins) tuples, sorted by games desc
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Get last played hero (most recent enriched match)
            cursor.execute(
                """
                SELECT mp.hero_id
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id = ? AND mp.guild_id = ? AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                ORDER BY m.match_date DESC
                LIMIT 1
                """,
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            last_hero_id = row["hero_id"] if row else None

            # Get hero frequency (games and wins per hero)
            cursor.execute(
                """
                SELECT mp.hero_id, COUNT(*) as games, SUM(CASE WHEN mp.won THEN 1 ELSE 0 END) as wins
                FROM match_participants mp
                WHERE mp.discord_id = ? AND mp.guild_id = ? AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                GROUP BY mp.hero_id
                ORDER BY games DESC
                LIMIT 5
                """,
                (discord_id, guild_id),
            )
            rows = cursor.fetchall()
            hero_counts = [(row["hero_id"], row["games"], row["wins"]) for row in rows]

            return {
                "last_hero_id": last_hero_id,
                "hero_counts": hero_counts,
            }

    def wipe_match_enrichment(self, match_id: int) -> bool:
        """
        Clear all enrichment data for a specific match.

        Returns True if match was found and wiped, False otherwise.
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Clear match-level enrichment
            cursor.execute(
                """
                UPDATE matches
                SET valve_match_id = NULL,
                    duration_seconds = NULL,
                    radiant_score = NULL,
                    dire_score = NULL,
                    game_mode = NULL,
                    enrichment_data = NULL,
                    enrichment_source = NULL,
                    enrichment_confidence = NULL
                WHERE match_id = ?
                """,
                (match_id,),
            )

            if cursor.rowcount == 0:
                return False

            # Clear participant-level stats (including fantasy fields)
            cursor.execute(
                """
                UPDATE match_participants
                SET hero_id = NULL,
                    kills = NULL,
                    deaths = NULL,
                    assists = NULL,
                    gpm = NULL,
                    xpm = NULL,
                    hero_damage = NULL,
                    tower_damage = NULL,
                    last_hits = NULL,
                    denies = NULL,
                    net_worth = NULL,
                    hero_healing = NULL,
                    lane_role = NULL,
                    lane_efficiency = NULL,
                    towers_killed = NULL,
                    roshans_killed = NULL,
                    teamfight_participation = NULL,
                    obs_placed = NULL,
                    sen_placed = NULL,
                    camps_stacked = NULL,
                    rune_pickups = NULL,
                    firstblood_claimed = NULL,
                    stuns = NULL,
                    fantasy_points = NULL
                WHERE match_id = ?
                """,
                (match_id,),
            )

            return True

    def wipe_auto_discovered_enrichments(self, guild_id: int | None = None) -> int:
        """
        Clear all enrichments that were auto-discovered for a guild.

        Returns count of matches wiped.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            # Get match IDs that are auto-discovered
            cursor.execute(
                "SELECT match_id FROM matches WHERE guild_id = ? AND enrichment_source = 'auto'",
                (normalized_guild,),
            )
            match_ids = [row["match_id"] for row in cursor.fetchall()]

            if not match_ids:
                return 0

            # Clear match-level enrichment
            cursor.execute(
                """
                UPDATE matches
                SET valve_match_id = NULL,
                    duration_seconds = NULL,
                    radiant_score = NULL,
                    dire_score = NULL,
                    game_mode = NULL,
                    enrichment_data = NULL,
                    enrichment_source = NULL,
                    enrichment_confidence = NULL
                WHERE guild_id = ? AND enrichment_source = 'auto'
                """,
                (normalized_guild,),
            )

            # Clear participant stats for those matches (including fantasy fields)
            placeholders = ",".join("?" * len(match_ids))
            cursor.execute(
                f"""
                UPDATE match_participants
                SET hero_id = NULL,
                    kills = NULL,
                    deaths = NULL,
                    assists = NULL,
                    gpm = NULL,
                    xpm = NULL,
                    hero_damage = NULL,
                    tower_damage = NULL,
                    last_hits = NULL,
                    denies = NULL,
                    net_worth = NULL,
                    hero_healing = NULL,
                    lane_role = NULL,
                    lane_efficiency = NULL,
                    towers_killed = NULL,
                    roshans_killed = NULL,
                    teamfight_participation = NULL,
                    obs_placed = NULL,
                    sen_placed = NULL,
                    camps_stacked = NULL,
                    rune_pickups = NULL,
                    firstblood_claimed = NULL,
                    stuns = NULL,
                    fantasy_points = NULL
                WHERE match_id IN ({placeholders})
                """,
                match_ids,
            )

            return len(match_ids)

    def get_auto_discovered_count(self, guild_id: int | None = None) -> int:
        """Get count of auto-discovered enriched matches."""
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COUNT(*) as count FROM matches WHERE guild_id = ? AND enrichment_source = 'auto'",
                (normalized_guild,),
            )
            return cursor.fetchone()["count"]

    def get_enriched_count(self, guild_id: int | None = None) -> int:
        """Get count of all enriched matches (any source)."""
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COUNT(*) as count FROM matches WHERE guild_id = ? AND valve_match_id IS NOT NULL",
                (normalized_guild,),
            )
            return cursor.fetchone()["count"]

    def wipe_all_enrichments(self, guild_id: int | None = None) -> int:
        """
        Clear ALL enrichments (both auto and manual) for a guild.

        Returns count of matches wiped.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            # Get match IDs that are enriched
            cursor.execute(
                "SELECT match_id FROM matches WHERE guild_id = ? AND valve_match_id IS NOT NULL",
                (normalized_guild,),
            )
            match_ids = [row["match_id"] for row in cursor.fetchall()]

            if not match_ids:
                return 0

            # Clear match-level enrichment
            cursor.execute(
                """
                UPDATE matches
                SET valve_match_id = NULL,
                    duration_seconds = NULL,
                    radiant_score = NULL,
                    dire_score = NULL,
                    game_mode = NULL,
                    enrichment_data = NULL,
                    enrichment_source = NULL,
                    enrichment_confidence = NULL
                WHERE guild_id = ? AND valve_match_id IS NOT NULL
                """,
                (normalized_guild,),
            )

            # Clear participant stats for those matches (including fantasy fields)
            placeholders = ",".join("?" * len(match_ids))
            cursor.execute(
                f"""
                UPDATE match_participants
                SET hero_id = NULL,
                    kills = NULL,
                    deaths = NULL,
                    assists = NULL,
                    gpm = NULL,
                    xpm = NULL,
                    hero_damage = NULL,
                    tower_damage = NULL,
                    last_hits = NULL,
                    denies = NULL,
                    net_worth = NULL,
                    hero_healing = NULL,
                    lane_role = NULL,
                    lane_efficiency = NULL,
                    towers_killed = NULL,
                    roshans_killed = NULL,
                    teamfight_participation = NULL,
                    obs_placed = NULL,
                    sen_placed = NULL,
                    camps_stacked = NULL,
                    rune_pickups = NULL,
                    firstblood_claimed = NULL,
                    stuns = NULL,
                    fantasy_points = NULL
                WHERE match_id IN ({placeholders})
                """,
                match_ids,
            )

            return len(match_ids)

    def get_biggest_upsets(self, guild_id: int | None, limit: int = 5) -> list[dict]:
        """Get matches where the underdog won, sorted by upset magnitude."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    mp.match_id,
                    mp.expected_radiant_win_prob,
                    mp.radiant_rating,
                    mp.dire_rating,
                    m.winning_team,
                    m.match_date,
                    m.team1_players,
                    m.team2_players
                FROM match_predictions mp
                JOIN matches m ON m.match_id = mp.match_id
                WHERE m.guild_id = ?
                  AND m.winning_team IS NOT NULL
                  AND (
                    (m.winning_team = 1 AND mp.expected_radiant_win_prob < 0.5)
                    OR (m.winning_team = 2 AND mp.expected_radiant_win_prob > 0.5)
                  )
                ORDER BY
                    CASE
                        WHEN m.winning_team = 1 THEN mp.expected_radiant_win_prob
                        ELSE 1.0 - mp.expected_radiant_win_prob
                    END ASC
                LIMIT ?
            """,
                (normalized_guild_id, limit),
            )
            rows = cursor.fetchall()
            results = []
            for row in rows:
                win_prob = row["expected_radiant_win_prob"]
                winning_team = row["winning_team"]
                # Calculate underdog's win probability
                underdog_prob = win_prob if winning_team == 1 else 1.0 - win_prob
                results.append({
                    "match_id": row["match_id"],
                    "underdog_win_prob": underdog_prob,
                    "radiant_rating": row["radiant_rating"],
                    "dire_rating": row["dire_rating"],
                    "winning_team": winning_team,
                    "match_date": row["match_date"],
                    "team1_players": json.loads(row["team1_players"]) if row["team1_players"] else [],
                    "team2_players": json.loads(row["team2_players"]) if row["team2_players"] else [],
                })
            return results

    def get_player_performance_stats(self, guild_id: int | None) -> list[dict]:
        """Get player expected vs actual win stats from rating history."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    discord_id,
                    COUNT(*) as total_matches,
                    SUM(CASE WHEN won = 1 THEN 1 ELSE 0 END) as actual_wins,
                    SUM(expected_team_win_prob) as expected_wins
                FROM rating_history
                WHERE guild_id = ? AND expected_team_win_prob IS NOT NULL
                GROUP BY discord_id
                HAVING COUNT(*) >= 5
                ORDER BY (SUM(CASE WHEN won = 1 THEN 1 ELSE 0 END) - SUM(expected_team_win_prob)) DESC
            """,
                (normalized_guild_id,),
            )
            rows = cursor.fetchall()
            return [
                {
                    "discord_id": row["discord_id"],
                    "total_matches": row["total_matches"],
                    "actual_wins": row["actual_wins"],
                    "expected_wins": row["expected_wins"],
                    "overperformance": row["actual_wins"] - row["expected_wins"],
                }
                for row in rows
            ]

    def get_lobby_type_stats(self, guild_id: int | None = None) -> list[dict]:
        """
        Get server-wide rating swing statistics by lobby type.

        Returns list of dicts with:
        - lobby_type: 'shuffle' or 'draft'
        - avg_swing: average absolute rating change
        - games: number of rating changes recorded
        - actual_win_rate: percentage of games won
        - expected_win_rate: average expected win probability
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    m.lobby_type,
                    AVG(ABS(rh.rating - rh.rating_before)) as avg_swing,
                    COUNT(*) as games,
                    AVG(CASE WHEN rh.won = 1 THEN 1.0 ELSE 0.0 END) as actual_win_rate,
                    AVG(rh.expected_team_win_prob) as expected_win_rate
                FROM rating_history rh
                JOIN matches m ON rh.match_id = m.match_id
                WHERE rh.guild_id = ? AND rh.rating_before IS NOT NULL
                GROUP BY m.lobby_type
                """,
                (normalized_guild,),
            )
            rows = cursor.fetchall()
            return [
                {
                    "lobby_type": row["lobby_type"] or "shuffle",
                    "avg_swing": row["avg_swing"],
                    "games": row["games"],
                    "actual_win_rate": row["actual_win_rate"],
                    "expected_win_rate": row["expected_win_rate"],
                }
                for row in rows
            ]

    def get_player_lobby_type_stats(self, discord_id: int, guild_id: int) -> list[dict]:
        """
        Get individual player's rating swing statistics by lobby type in a guild.

        Same return format as get_lobby_type_stats but filtered to one player.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    m.lobby_type,
                    AVG(ABS(rh.rating - rh.rating_before)) as avg_swing,
                    COUNT(*) as games,
                    AVG(CASE WHEN rh.won = 1 THEN 1.0 ELSE 0.0 END) as actual_win_rate,
                    AVG(rh.expected_team_win_prob) as expected_win_rate
                FROM rating_history rh
                JOIN matches m ON rh.match_id = m.match_id
                WHERE rh.rating_before IS NOT NULL AND rh.discord_id = ? AND rh.guild_id = ?
                GROUP BY m.lobby_type
                """,
                (discord_id, guild_id),
            )
            rows = cursor.fetchall()
            return [
                {
                    "lobby_type": row["lobby_type"] or "shuffle",
                    "avg_swing": row["avg_swing"],
                    "games": row["games"],
                    "actual_win_rate": row["actual_win_rate"],
                    "expected_win_rate": row["expected_win_rate"],
                }
                for row in rows
            ]

    def get_player_hero_stats_detailed(self, discord_id: int, guild_id: int, limit: int = 10) -> list[dict]:
        """
        Get a player's recent hero performance from enriched matches in a guild.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            limit: Maximum number of hero performances to return

        Returns:
            List of dicts with hero_id, wins, losses, avg_kda, avg_gpm, avg_damage,
            ordered by most recent match.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    mp.hero_id,
                    COUNT(*) as games,
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN mp.won = 0 THEN 1 ELSE 0 END) as losses,
                    AVG(mp.kills) as avg_kills,
                    AVG(mp.deaths) as avg_deaths,
                    AVG(mp.assists) as avg_assists,
                    AVG(mp.gpm) as avg_gpm,
                    AVG(mp.hero_damage) as avg_damage,
                    MAX(mp.match_id) as last_match_id
                FROM match_participants mp
                WHERE mp.discord_id = ? AND mp.guild_id = ? AND mp.hero_id IS NOT NULL
                GROUP BY mp.hero_id
                ORDER BY last_match_id DESC
                LIMIT ?
                """,
                (discord_id, guild_id, limit),
            )
            rows = cursor.fetchall()
            return [
                {
                    "hero_id": row["hero_id"],
                    "games": row["games"],
                    "wins": row["wins"],
                    "losses": row["losses"],
                    "avg_kills": row["avg_kills"] or 0,
                    "avg_deaths": row["avg_deaths"] or 0,
                    "avg_assists": row["avg_assists"] or 0,
                    "avg_gpm": row["avg_gpm"] or 0,
                    "avg_damage": row["avg_damage"] or 0,
                }
                for row in rows
            ]

    def get_player_hero_role_breakdown(self, discord_id: int, guild_id: int) -> dict:
        """
        Get breakdown of core vs support heroes played by a player in a guild.

        Returns:
            Dict with total_games, core_games, support_games based on hero_id classification.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT hero_id, COUNT(*) as games
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ? AND hero_id IS NOT NULL
                GROUP BY hero_id
                """,
                (discord_id, guild_id),
            )
            rows = cursor.fetchall()
            return [{"hero_id": row["hero_id"], "games": row["games"]} for row in rows]

    def get_player_fantasy_stats(self, discord_id: int, guild_id: int) -> dict:
        """
        Get fantasy point statistics for a player from enriched matches in a guild.

        Returns:
            Dict with:
            - total_games: int (games with fantasy data)
            - total_fp: float (total fantasy points)
            - avg_fp: float (average fantasy points per game)
            - best_fp: float (highest fantasy points in a game)
            - best_match_id: int (match_id of best game)
            - recent_games: list of (match_id, fantasy_points, won) for last 10 games
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Get aggregate stats
            cursor.execute(
                """
                SELECT
                    COUNT(*) as total_games,
                    SUM(fantasy_points) as total_fp,
                    AVG(fantasy_points) as avg_fp,
                    MAX(fantasy_points) as best_fp
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ? AND fantasy_points IS NOT NULL
                """,
                (discord_id, guild_id),
            )
            row = cursor.fetchone()

            if not row or row["total_games"] == 0:
                return {
                    "total_games": 0,
                    "total_fp": 0.0,
                    "avg_fp": 0.0,
                    "best_fp": 0.0,
                    "best_match_id": None,
                    "recent_games": [],
                }

            # Get best match
            cursor.execute(
                """
                SELECT match_id
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ? AND fantasy_points = ?
                LIMIT 1
                """,
                (discord_id, guild_id, row["best_fp"]),
            )
            best_row = cursor.fetchone()
            best_match_id = best_row["match_id"] if best_row else None

            # Get recent games with fantasy data
            cursor.execute(
                """
                SELECT mp.match_id, mp.fantasy_points, mp.won, mp.hero_id
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id = ? AND mp.guild_id = ? AND mp.fantasy_points IS NOT NULL
                ORDER BY m.match_date DESC
                LIMIT 10
                """,
                (discord_id, guild_id),
            )
            recent = [
                {
                    "match_id": r["match_id"],
                    "fantasy_points": r["fantasy_points"],
                    "won": r["won"],
                    "hero_id": r["hero_id"],
                }
                for r in cursor.fetchall()
            ]

            return {
                "total_games": row["total_games"],
                "total_fp": row["total_fp"] or 0.0,
                "avg_fp": row["avg_fp"] or 0.0,
                "best_fp": row["best_fp"] or 0.0,
                "best_match_id": best_match_id,
                "recent_games": recent,
            }

    def get_matches_without_fantasy_data(self, limit: int = 100) -> list[dict]:
        """
        Get matches that have enrichment but no fantasy data.

        Returns matches where valve_match_id is set but fantasy_points is NULL.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT DISTINCT m.match_id, m.valve_match_id, m.match_date
                FROM matches m
                JOIN match_participants mp ON m.match_id = mp.match_id
                WHERE m.valve_match_id IS NOT NULL
                  AND mp.fantasy_points IS NULL
                ORDER BY m.match_date DESC
                LIMIT ?
                """,
                (limit,),
            )
            rows = cursor.fetchall()
            return [
                {
                    "match_id": row["match_id"],
                    "valve_match_id": row["valve_match_id"],
                    "match_date": row["match_date"],
                }
                for row in rows
            ]

    def get_enriched_matches_chronological(self, guild_id: int | None = None) -> list[dict]:
        """
        Get all matches with fantasy data in chronological order for backfill.

        Returns matches ordered by match_date ASC (oldest first) where at least
        one participant has fantasy_points set.

        Returns:
            List of dicts with match_id, winning_team, match_date, team1_players, team2_players
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT DISTINCT m.match_id, m.winning_team, m.match_date,
                       m.team1_players, m.team2_players
                FROM matches m
                JOIN match_participants mp ON m.match_id = mp.match_id
                WHERE m.guild_id = ? AND mp.fantasy_points IS NOT NULL
                ORDER BY m.match_date ASC
                """,
                (normalized_guild,),
            )
            rows = cursor.fetchall()
            return [
                {
                    "match_id": row["match_id"],
                    "winning_team": row["winning_team"],
                    "match_date": row["match_date"],
                    "team1_players": json.loads(row["team1_players"]),
                    "team2_players": json.loads(row["team2_players"]),
                }
                for row in rows
            ]

    def get_all_matches_chronological(self, guild_id: int | None = None) -> list[dict]:
        """
        Get ALL matches in chronological order for backfill.

        Unlike get_enriched_matches_chronological, this returns ALL matches
        regardless of whether they have fantasy data. Used for full backfill
        where non-enriched matches use equal weights.

        Returns:
            List of dicts with match_id, winning_team, match_date, team1_players, team2_players
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT match_id, winning_team, match_date,
                       team1_players, team2_players
                FROM matches
                WHERE guild_id = ? AND winning_team IN (1, 2)
                ORDER BY match_date ASC
                """,
                (normalized_guild,),
            )
            rows = cursor.fetchall()
            return [
                {
                    "match_id": row["match_id"],
                    "winning_team": row["winning_team"],
                    "match_date": row["match_date"],
                    "team1_players": json.loads(row["team1_players"]),
                    "team2_players": json.loads(row["team2_players"]),
                }
                for row in rows
            ]

    def get_all_matches_with_predictions(self, guild_id: int | None = None) -> list[dict]:
        """
        Get all matches with Glicko-2 prediction data for analysis.

        Returns matches with expected_radiant_win_prob from match_predictions,
        along with actual outcome and team info.

        Returns:
            List of dicts with match data and prediction info
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT m.match_id, m.winning_team, m.match_date,
                       m.team1_players, m.team2_players,
                       mp.expected_radiant_win_prob,
                       mp.radiant_rating, mp.dire_rating,
                       mp.radiant_rd, mp.dire_rd
                FROM matches m
                JOIN match_predictions mp ON m.match_id = mp.match_id
                WHERE m.guild_id = ?
                ORDER BY m.match_date ASC
                """,
                (normalized_guild,),
            )
            rows = cursor.fetchall()
            return [
                {
                    "match_id": row["match_id"],
                    "winning_team": row["winning_team"],
                    "match_date": row["match_date"],
                    "team1_players": json.loads(row["team1_players"]),
                    "team2_players": json.loads(row["team2_players"]),
                    "expected_radiant_win_prob": row["expected_radiant_win_prob"],
                    "radiant_rating": row["radiant_rating"],
                    "dire_rating": row["dire_rating"],
                    "radiant_rd": row["radiant_rd"],
                    "dire_rd": row["dire_rd"],
                }
                for row in rows
            ]

    def get_player_openskill_history(self, discord_id: int, guild_id: int, limit: int = 10) -> list[dict]:
        """
        Get a player's recent OpenSkill rating changes in a guild.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            limit: Maximum number of history entries to return

        Returns:
            List of dicts with os_mu_before, os_mu_after, os_sigma_before, os_sigma_after,
            fantasy_weight, won, match_id, ordered by most recent first.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT rh.os_mu_before, rh.os_mu_after, rh.os_sigma_before, rh.os_sigma_after,
                       rh.fantasy_weight, rh.won, rh.match_id, m.match_date
                FROM rating_history rh
                JOIN matches m ON rh.match_id = m.match_id
                WHERE rh.discord_id = ? AND rh.guild_id = ?
                  AND rh.os_mu_before IS NOT NULL
                  AND rh.os_mu_after IS NOT NULL
                ORDER BY m.match_date DESC
                LIMIT ?
                """,
                (discord_id, guild_id, limit),
            )
            rows = cursor.fetchall()
            return [
                {
                    "os_mu_before": row["os_mu_before"],
                    "os_mu_after": row["os_mu_after"],
                    "os_sigma_before": row["os_sigma_before"],
                    "os_sigma_after": row["os_sigma_after"],
                    "fantasy_weight": row["fantasy_weight"],
                    "won": row["won"],
                    "match_id": row["match_id"],
                    "match_date": row["match_date"],
                }
                for row in rows
            ]

    def update_rating_history_openskill_bulk(
        self, match_id: int, updates: list[dict]
    ) -> int:
        """
        Bulk update rating_history entries with OpenSkill data.

        Args:
            match_id: The match ID
            updates: List of dicts with keys:
                - discord_id (required)
                - os_mu_before, os_mu_after, os_sigma_before, os_sigma_after, fantasy_weight

        Returns:
            Number of rows updated
        """
        if not updates:
            return 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.executemany(
                """
                UPDATE rating_history
                SET os_mu_before = ?,
                    os_mu_after = ?,
                    os_sigma_before = ?,
                    os_sigma_after = ?,
                    fantasy_weight = ?
                WHERE match_id = ? AND discord_id = ?
                """,
                [
                    (
                        u.get("os_mu_before"),
                        u.get("os_mu_after"),
                        u.get("os_sigma_before"),
                        u.get("os_sigma_after"),
                        u.get("fantasy_weight"),
                        match_id,
                        u["discord_id"],
                    )
                    for u in updates
                ],
            )
            return cursor.rowcount

    def get_os_baseline_for_match(self, match_id: int) -> dict[int, tuple[float, float]]:
        """
        Get os_mu_before/os_sigma_before from rating_history for Phase 2 recalculation.

        This retrieves the baseline OpenSkill values that were stored during Phase 1
        (equal-weight update at match recording). Phase 2 uses these as the starting
        point for fantasy-weighted recalculation.

        Args:
            match_id: The match ID to look up

        Returns:
            Dict mapping discord_id -> (os_mu_before, os_sigma_before)
            Empty dict if no baseline data exists
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, os_mu_before, os_sigma_before
                FROM rating_history
                WHERE match_id = ?
                  AND os_mu_before IS NOT NULL
                  AND os_sigma_before IS NOT NULL
                """,
                (match_id,),
            )
            rows = cursor.fetchall()
            return {
                row["discord_id"]: (row["os_mu_before"], row["os_sigma_before"])
                for row in rows
            }

    def get_full_rating_history_for_match(self, match_id: int) -> list[dict]:
        """
        Get all rating history entries for a specific match with full snapshot data.

        Used for match correction to restore pre-match ratings.

        Returns:
            List of dicts with all rating history columns including before/after values
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    discord_id,
                    rating,
                    rating_before,
                    rd_before,
                    rd_after,
                    volatility_before,
                    volatility_after,
                    expected_team_win_prob,
                    team_number,
                    won,
                    os_mu_before,
                    os_mu_after,
                    os_sigma_before,
                    os_sigma_after,
                    fantasy_weight
                FROM rating_history
                WHERE match_id = ?
                """,
                (match_id,),
            )
            rows = cursor.fetchall()
            return [dict(row) for row in rows]

    def update_match_result(self, match_id: int, new_winning_team: int) -> None:
        """
        Update the winning_team for a match and update match_participants.won accordingly.

        Args:
            match_id: The match ID to update
            new_winning_team: 1 for Radiant, 2 for Dire
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            # Update match winning_team
            cursor.execute(
                "UPDATE matches SET winning_team = ? WHERE match_id = ?",
                (new_winning_team, match_id),
            )
            # Update participants: team 1 = radiant, team 2 = dire
            cursor.execute(
                """
                UPDATE match_participants
                SET won = CASE
                    WHEN team_number = ? THEN 1
                    ELSE 0
                END
                WHERE match_id = ?
                """,
                (new_winning_team, match_id),
            )

    def update_rating_history_for_correction(
        self,
        match_id: int,
        discord_id: int,
        new_rating: float,
        new_rd: float,
        new_volatility: float,
        new_won: bool,
        new_os_mu: float | None = None,
        new_os_sigma: float | None = None,
    ) -> None:
        """
        Update rating history entry for a match correction.

        Updates the 'after' values while preserving 'before' snapshots.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE rating_history
                SET rating = ?,
                    rd_after = ?,
                    volatility_after = ?,
                    won = ?,
                    os_mu_after = COALESCE(?, os_mu_after),
                    os_sigma_after = COALESCE(?, os_sigma_after)
                WHERE match_id = ? AND discord_id = ?
                """,
                (
                    new_rating,
                    new_rd,
                    new_volatility,
                    new_won,
                    new_os_mu,
                    new_os_sigma,
                    match_id,
                    discord_id,
                ),
            )

    def add_match_correction(
        self,
        match_id: int,
        old_winning_team: int,
        new_winning_team: int,
        corrected_by: int,
    ) -> int:
        """
        Log a match correction for audit purposes.

        Returns:
            The correction_id
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO match_corrections
                (match_id, old_winning_team, new_winning_team, corrected_by)
                VALUES (?, ?, ?, ?)
                """,
                (match_id, old_winning_team, new_winning_team, corrected_by),
            )
            return cursor.lastrowid

    def get_match_corrections(self, match_id: int) -> list[dict]:
        """
        Get correction history for a match.

        Returns:
            List of correction records with correction_id, old/new winning_team,
            corrected_by, and corrected_at
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT correction_id, old_winning_team, new_winning_team,
                       corrected_by, corrected_at
                FROM match_corrections
                WHERE match_id = ?
                ORDER BY corrected_at ASC
                """,
                (match_id,),
            )
            return [dict(row) for row in cursor.fetchall()]

    # -------------------------------------------------------------------------
    # Hero Stats Methods for Profile Heroes Tab
    # -------------------------------------------------------------------------

    def get_player_hero_detailed_stats(
        self, discord_id: int, guild_id: int | None = None, limit: int = 20
    ) -> list[dict]:
        """
        Get comprehensive per-hero stats from enriched matches.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)
            limit: Maximum number of heroes to return

        Returns:
            List of dicts with hero_id, games, wins, avg stats, ordered by games desc
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT hero_id,
                       COUNT(*) as games,
                       SUM(CASE WHEN won THEN 1 ELSE 0 END) as wins,
                       AVG(kills) as avg_kills,
                       AVG(deaths) as avg_deaths,
                       AVG(assists) as avg_assists,
                       AVG(gpm) as avg_gpm,
                       AVG(xpm) as avg_xpm,
                       AVG(hero_damage) as avg_damage,
                       AVG(tower_damage) as avg_tower_damage,
                       AVG(fantasy_points) as avg_fantasy,
                       AVG(lane_efficiency) as avg_lane_eff,
                       SUM(COALESCE(obs_placed, 0)) as total_obs,
                       SUM(COALESCE(sen_placed, 0)) as total_sens
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ? AND hero_id IS NOT NULL AND hero_id > 0
                GROUP BY hero_id
                ORDER BY games DESC
                LIMIT ?
                """,
                (discord_id, normalized_guild_id, limit),
            )
            rows = cursor.fetchall()
            return [
                {
                    "hero_id": row["hero_id"],
                    "games": row["games"],
                    "wins": row["wins"],
                    "avg_kills": row["avg_kills"] or 0,
                    "avg_deaths": row["avg_deaths"] or 0,
                    "avg_assists": row["avg_assists"] or 0,
                    "avg_gpm": row["avg_gpm"] or 0,
                    "avg_xpm": row["avg_xpm"] or 0,
                    "avg_damage": row["avg_damage"] or 0,
                    "avg_tower_damage": row["avg_tower_damage"] or 0,
                    "avg_fantasy": row["avg_fantasy"] or 0,
                    "avg_lane_eff": row["avg_lane_eff"],
                    "total_obs": row["total_obs"] or 0,
                    "total_sens": row["total_sens"] or 0,
                }
                for row in rows
            ]

    def get_player_lane_stats(self, discord_id: int, guild_id: int | None = None) -> list[dict]:
        """
        Get performance by lane from enriched matches.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)

        Returns:
            List of dicts with lane_role, games, wins, avg stats
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT lane_role,
                       COUNT(*) as games,
                       SUM(CASE WHEN won THEN 1 ELSE 0 END) as wins,
                       AVG(kills) as avg_kills,
                       AVG(deaths) as avg_deaths,
                       AVG(assists) as avg_assists,
                       AVG(gpm) as avg_gpm,
                       AVG(xpm) as avg_xpm,
                       AVG(lane_efficiency) as avg_lane_eff
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ? AND lane_role IS NOT NULL
                GROUP BY lane_role
                ORDER BY games DESC
                """,
                (discord_id, normalized_guild_id),
            )
            rows = cursor.fetchall()
            return [
                {
                    "lane_role": row["lane_role"],
                    "games": row["games"],
                    "wins": row["wins"],
                    "avg_kills": row["avg_kills"] or 0,
                    "avg_deaths": row["avg_deaths"] or 0,
                    "avg_assists": row["avg_assists"] or 0,
                    "avg_gpm": row["avg_gpm"] or 0,
                    "avg_xpm": row["avg_xpm"] or 0,
                    "avg_lane_eff": row["avg_lane_eff"],
                }
                for row in rows
            ]

    def get_player_ward_stats_by_lane(
        self, discord_id: int, guild_id: int | None = None
    ) -> list[dict]:
        """
        Get ward stats by lane from enriched matches.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)

        Returns:
            List of dicts with lane_role, games, total/avg obs/sens
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT lane_role,
                       COUNT(*) as games,
                       SUM(COALESCE(obs_placed, 0)) as total_obs,
                       SUM(COALESCE(sen_placed, 0)) as total_sens,
                       AVG(COALESCE(obs_placed, 0)) as avg_obs,
                       AVG(COALESCE(sen_placed, 0)) as avg_sens
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ?
                  AND (obs_placed IS NOT NULL OR sen_placed IS NOT NULL)
                GROUP BY lane_role
                ORDER BY lane_role
                """,
                (discord_id, normalized_guild_id),
            )
            rows = cursor.fetchall()
            return [
                {
                    "lane_role": row["lane_role"],
                    "games": row["games"],
                    "total_obs": row["total_obs"] or 0,
                    "total_sens": row["total_sens"] or 0,
                    "avg_obs": row["avg_obs"] or 0,
                    "avg_sens": row["avg_sens"] or 0,
                }
                for row in rows
            ]

    def get_player_hero_lane_performance(
        self, discord_id: int, guild_id: int | None = None
    ) -> list[dict]:
        """
        Get per-hero lane performance from enriched matches.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)

        Returns:
            List of dicts with hero_id, lane_role, games, wins, avg stats
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT hero_id, lane_role,
                       COUNT(*) as games,
                       SUM(CASE WHEN won THEN 1 ELSE 0 END) as wins,
                       AVG(lane_efficiency) as avg_lane_eff,
                       AVG(gpm) as avg_gpm
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ?
                  AND hero_id IS NOT NULL AND hero_id > 0
                  AND lane_role IS NOT NULL
                GROUP BY hero_id, lane_role
                ORDER BY hero_id, games DESC
                """,
                (discord_id, normalized_guild_id),
            )
            rows = cursor.fetchall()
            return [
                {
                    "hero_id": row["hero_id"],
                    "lane_role": row["lane_role"],
                    "games": row["games"],
                    "wins": row["wins"],
                    "avg_lane_eff": row["avg_lane_eff"],
                    "avg_gpm": row["avg_gpm"] or 0,
                }
                for row in rows
            ]

    def get_player_hero_vs_opponent_heroes(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 2
    ) -> list[dict]:
        """
        Get hero vs opponent hero matchups from enriched matches.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)
            min_games: Minimum games for a matchup to be included

        Returns:
            List of dicts with my_hero, opponent_hero, games, wins
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT mp.hero_id as my_hero,
                       opp.hero_id as opponent_hero,
                       COUNT(*) as games,
                       SUM(CASE WHEN mp.won THEN 1 ELSE 0 END) as wins
                FROM match_participants mp
                JOIN match_participants opp ON mp.match_id = opp.match_id
                     AND mp.team_number != opp.team_number
                WHERE mp.discord_id = ? AND mp.guild_id = ?
                  AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                  AND opp.hero_id IS NOT NULL AND opp.hero_id > 0
                GROUP BY mp.hero_id, opp.hero_id
                HAVING COUNT(*) >= ?
                ORDER BY games DESC
                LIMIT 30
                """,
                (discord_id, normalized_guild_id, min_games),
            )
            rows = cursor.fetchall()
            return [
                {
                    "my_hero": row["my_hero"],
                    "opponent_hero": row["opponent_hero"],
                    "games": row["games"],
                    "wins": row["wins"],
                }
                for row in rows
            ]

    def get_player_enriched_match_count(self, discord_id: int, guild_id: int | None = None) -> int:
        """
        Get count of enriched matches with hero data for a player.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by

        Returns:
            Count of matches with hero_id set
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COUNT(*) as count
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id = ? AND m.guild_id = ? AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                """,
                (discord_id, normalized_guild),
            )
            return cursor.fetchone()["count"]

    def get_player_overall_hero_stats(
        self, discord_id: int, guild_id: int | None = None
    ) -> dict:
        """
        Get aggregated hero stats for a player (for tab header).

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)

        Returns:
            Dict with total_games, avg_kills, avg_deaths, avg_assists, etc.
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COUNT(*) as total_games,
                       AVG(kills) as avg_kills,
                       AVG(deaths) as avg_deaths,
                       AVG(assists) as avg_assists,
                       AVG(gpm) as avg_gpm,
                       AVG(xpm) as avg_xpm,
                       AVG(fantasy_points) as avg_fantasy,
                       SUM(COALESCE(obs_placed, 0)) as total_obs,
                       SUM(COALESCE(sen_placed, 0)) as total_sens
                FROM match_participants
                WHERE discord_id = ? AND guild_id = ? AND hero_id IS NOT NULL AND hero_id > 0
                """,
                (discord_id, normalized_guild_id),
            )
            row = cursor.fetchone()
            return {
                "total_games": row["total_games"] or 0,
                "avg_kills": row["avg_kills"] or 0,
                "avg_deaths": row["avg_deaths"] or 0,
                "avg_assists": row["avg_assists"] or 0,
                "avg_gpm": row["avg_gpm"] or 0,
                "avg_xpm": row["avg_xpm"] or 0,
                "avg_fantasy": row["avg_fantasy"] or 0,
                "total_obs": row["total_obs"] or 0,
                "total_sens": row["total_sens"] or 0,
            }

    def get_player_nemesis_heroes(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 2
    ) -> list[dict]:
        """
        Get heroes on enemy team that the player loses to most often.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)
            min_games: Minimum games against to be included

        Returns:
            List of dicts with enemy_hero, games, wins, losses, loss_rate
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT opp.hero_id as enemy_hero,
                       COUNT(*) as games,
                       SUM(CASE WHEN mp.won = 0 THEN 1 ELSE 0 END) as losses,
                       SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins
                FROM match_participants mp
                JOIN match_participants opp ON mp.match_id = opp.match_id
                     AND mp.team_number != opp.team_number
                WHERE mp.discord_id = ? AND mp.guild_id = ?
                  AND opp.hero_id IS NOT NULL AND opp.hero_id > 0
                GROUP BY opp.hero_id
                HAVING COUNT(*) >= ?
                ORDER BY (losses * 1.0 / games) DESC, games DESC
                LIMIT 10
                """,
                (discord_id, normalized_guild_id, min_games),
            )
            rows = cursor.fetchall()
            return [
                {
                    "enemy_hero": row["enemy_hero"],
                    "games": row["games"],
                    "wins": row["wins"],
                    "losses": row["losses"],
                    "loss_rate": row["losses"] / row["games"] if row["games"] > 0 else 0,
                }
                for row in rows
            ]

    def get_player_easiest_opponents(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 2
    ) -> list[dict]:
        """
        Get heroes on enemy team that the player wins against most often.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)
            min_games: Minimum games against to be included

        Returns:
            List of dicts with enemy_hero, games, wins, losses, win_rate
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT opp.hero_id as enemy_hero,
                       COUNT(*) as games,
                       SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins,
                       SUM(CASE WHEN mp.won = 0 THEN 1 ELSE 0 END) as losses
                FROM match_participants mp
                JOIN match_participants opp ON mp.match_id = opp.match_id
                     AND mp.team_number != opp.team_number
                WHERE mp.discord_id = ? AND mp.guild_id = ?
                  AND opp.hero_id IS NOT NULL AND opp.hero_id > 0
                GROUP BY opp.hero_id
                HAVING COUNT(*) >= ?
                ORDER BY (wins * 1.0 / games) DESC, games DESC
                LIMIT 10
                """,
                (discord_id, normalized_guild_id, min_games),
            )
            rows = cursor.fetchall()
            return [
                {
                    "enemy_hero": row["enemy_hero"],
                    "games": row["games"],
                    "wins": row["wins"],
                    "losses": row["losses"],
                    "win_rate": row["wins"] / row["games"] if row["games"] > 0 else 0,
                }
                for row in rows
            ]

    def get_player_best_hero_synergies(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 2
    ) -> list[dict]:
        """
        Get heroes on same team (teammates' heroes) player wins with most.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID to filter by (None normalizes to 0)
            min_games: Minimum games with to be included

        Returns:
            List of dicts with ally_hero, games, wins, win_rate
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT ally.hero_id as ally_hero,
                       COUNT(*) as games,
                       SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins
                FROM match_participants mp
                JOIN match_participants ally ON mp.match_id = ally.match_id
                     AND mp.team_number = ally.team_number
                     AND mp.discord_id != ally.discord_id
                WHERE mp.discord_id = ? AND mp.guild_id = ?
                  AND ally.hero_id IS NOT NULL AND ally.hero_id > 0
                GROUP BY ally.hero_id
                HAVING COUNT(*) >= ?
                ORDER BY (wins * 1.0 / games) DESC, games DESC
                LIMIT 10
                """,
                (discord_id, normalized_guild_id, min_games),
            )
            rows = cursor.fetchall()
            return [
                {
                    "ally_hero": row["ally_hero"],
                    "games": row["games"],
                    "wins": row["wins"],
                    "win_rate": row["wins"] / row["games"] if row["games"] > 0 else 0,
                }
                for row in rows
            ]

    def get_multi_player_hero_stats(self, discord_ids: list[int], guild_id: int | None = None) -> list[dict]:
        """
        Get hero stats for multiple players in a single query.

        Args:
            discord_ids: List of Discord IDs to query
            guild_id: Guild ID to filter by

        Returns:
            List of dicts with discord_id, hero_id, games, wins
            sorted by discord_id, games DESC
        """
        if not discord_ids:
            return []

        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT mp.discord_id, mp.hero_id,
                       COUNT(*) as games,
                       SUM(CASE WHEN mp.won THEN 1 ELSE 0 END) as wins
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id IN ({placeholders})
                  AND m.guild_id = ?
                  AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                GROUP BY mp.discord_id, mp.hero_id
                ORDER BY mp.discord_id, games DESC
                """,
                discord_ids + [normalized_guild],
            )
            rows = cursor.fetchall()
            return [
                {
                    "discord_id": row["discord_id"],
                    "hero_id": row["hero_id"],
                    "games": row["games"],
                    "wins": row["wins"],
                }
                for row in rows
            ]

    def get_players_with_enriched_data(self, guild_id: int | None = None) -> list[dict]:
        """
        Get all players that have at least one enriched match with hero data.

        Returns:
            List of dicts with discord_id, total_games ordered by total_games DESC
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT mp.discord_id, COUNT(*) as total_games
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE m.guild_id = ? AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                GROUP BY mp.discord_id
                ORDER BY total_games DESC
                """,
                (normalized_guild,),
            )
            rows = cursor.fetchall()
            return [
                {"discord_id": row["discord_id"], "total_games": row["total_games"]}
                for row in rows
            ]

    # -------------------------------------------------------------------------
    # Scout Command Methods
    # -------------------------------------------------------------------------

    def get_player_hero_stats_for_scout(
        self, discord_ids: list[int], guild_id: int | None = None
    ) -> dict[int, list[dict]]:
        """
        Get hero stats for multiple players, organized by player.

        Returns per-player hero stats including games, wins, losses, and primary role.
        Uses MODE() to find the most common lane_role for each hero.

        Args:
            discord_ids: List of Discord IDs to query
            guild_id: Guild ID to filter by

        Returns:
            Dict mapping discord_id -> list of {hero_id, games, wins, losses, primary_role}
            Each list is sorted by games descending.
        """
        if not discord_ids:
            return {}

        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            # Get hero stats with lane_role for each match
            cursor.execute(
                f"""
                SELECT mp.discord_id, mp.hero_id, mp.lane_role,
                       mp.won
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id IN ({placeholders})
                  AND m.guild_id = ?
                  AND mp.guild_id = ?
                  AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                """,
                discord_ids + [normalized_guild, normalized_guild],
            )
            rows = cursor.fetchall()

            # Aggregate in Python to compute mode of lane_role
            # Structure: {discord_id: {hero_id: {"games": N, "wins": N, "roles": []}}}
            player_hero_data: dict[int, dict[int, dict]] = {}

            for row in rows:
                discord_id = row["discord_id"]
                hero_id = row["hero_id"]
                lane_role = row["lane_role"]
                won = row["won"]

                if discord_id not in player_hero_data:
                    player_hero_data[discord_id] = {}

                if hero_id not in player_hero_data[discord_id]:
                    player_hero_data[discord_id][hero_id] = {
                        "games": 0,
                        "wins": 0,
                        "roles": [],
                    }

                player_hero_data[discord_id][hero_id]["games"] += 1
                if won:
                    player_hero_data[discord_id][hero_id]["wins"] += 1
                if lane_role is not None:
                    player_hero_data[discord_id][hero_id]["roles"].append(lane_role)

            # Convert to output format
            result: dict[int, list[dict]] = {}
            for discord_id, hero_stats in player_hero_data.items():
                heroes = []
                for hero_id, stats in hero_stats.items():
                    games = stats["games"]
                    wins = stats["wins"]
                    losses = games - wins
                    roles = stats["roles"]

                    # Compute mode of roles (most common lane_role)
                    if roles:
                        primary_role = max(set(roles), key=roles.count)
                    else:
                        primary_role = 1  # Default to carry if no data

                    heroes.append({
                        "hero_id": hero_id,
                        "games": games,
                        "wins": wins,
                        "losses": losses,
                        "primary_role": primary_role,
                    })

                # Sort by games descending
                heroes.sort(key=lambda x: x["games"], reverse=True)
                result[discord_id] = heroes

            return result

    def get_bans_for_players(
        self, discord_ids: list[int], guild_id: int | None = None
    ) -> dict[int, int]:
        """
        Extract opposing-team ban data from enrichment_data for matches where players participated.

        Only counts bans made by the opposing team (i.e. bans targeted against the scouted
        players). Each match is only counted once even if multiple players from the list
        were in the same match.

        Team mapping:
        - Our team_number=1 (Radiant) → OpenDota team=0
        - Our team_number=2 (Dire) → OpenDota team=1

        Args:
            discord_ids: List of Discord IDs to check
            guild_id: Guild ID to filter by

        Returns:
            Dict mapping hero_id -> ban_count (aggregated across all matches)
        """
        if not discord_ids:
            return {}

        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))

            # Get matches with enrichment data and the team_number of each scouted player
            cursor.execute(
                f"""
                SELECT m.match_id, m.enrichment_data, mp.team_number
                FROM matches m
                JOIN match_participants mp ON m.match_id = mp.match_id
                WHERE mp.discord_id IN ({placeholders})
                  AND m.guild_id = ?
                  AND mp.guild_id = ?
                  AND m.enrichment_data IS NOT NULL
                """,
                discord_ids + [normalized_guild, normalized_guild],
            )
            rows = cursor.fetchall()

            # Group by match_id: collect enrichment_data and team_numbers of scouted players
            match_data: dict[int, dict] = {}
            for row in rows:
                match_id = row["match_id"]
                if match_id not in match_data:
                    match_data[match_id] = {
                        "enrichment_data": row["enrichment_data"],
                        "team_numbers": set(),
                    }
                match_data[match_id]["team_numbers"].add(row["team_number"])

            # Parse enrichment_data and count only opposing-team bans
            ban_counts: dict[int, int] = {}

            for match_id, info in match_data.items():
                enrichment_data = info["enrichment_data"]
                team_numbers = info["team_numbers"]
                if not enrichment_data:
                    continue

                # Determine which OpenDota ban teams are "opposing"
                # Our team_number 1 (Radiant) → opposing OpenDota team is 1 (Dire)
                # Our team_number 2 (Dire) → opposing OpenDota team is 0 (Radiant)
                opposing_ban_teams: set[int] = set()
                for tn in team_numbers:
                    if tn == 1:
                        opposing_ban_teams.add(1)  # Dire bans
                    elif tn == 2:
                        opposing_ban_teams.add(0)  # Radiant bans

                try:
                    data = json.loads(enrichment_data)
                    picks_bans = data.get("picks_bans", [])

                    for entry in picks_bans:
                        if entry.get("is_pick") is False:
                            ban_team = entry.get("team")
                            if ban_team in opposing_ban_teams:
                                hero_id = entry.get("hero_id")
                                if hero_id:
                                    ban_counts[hero_id] = ban_counts.get(hero_id, 0) + 1
                except (json.JSONDecodeError, TypeError, KeyError):
                    continue

            return ban_counts

    def get_match_count_for_players(
        self, discord_ids: list[int], guild_id: int | None = None
    ) -> int:
        """
        Count unique matches where any of the specified players participated.

        Args:
            discord_ids: List of Discord IDs to check
            guild_id: Guild ID to filter by

        Returns:
            Total number of unique matches
        """
        if not discord_ids:
            return 0

        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT COUNT(DISTINCT mp.match_id) as match_count
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id IN ({placeholders})
                  AND m.guild_id = ?
                  AND mp.guild_id = ?
                """,
                discord_ids + [normalized_guild, normalized_guild],
            )
            row = cursor.fetchone()
            return row["match_count"] if row else 0
