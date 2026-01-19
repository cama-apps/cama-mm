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
        radiant_team_ids: list[int] | None = None,
        dire_team_ids: list[int] | None = None,
        dotabuff_match_id: str | None = None,
        notes: str | None = None,
        lobby_type: str = "shuffle",
    ) -> int:
        """
        Record a match result.

        Convention: team1 = Radiant, team2 = Dire.
        winning_team: 1 = Radiant won, 2 = Dire won.

        Args:
            team1_ids: Discord IDs of Radiant players
            team2_ids: Discord IDs of Dire players
            winning_team: 1 (Radiant won) or 2 (Dire won)
            radiant_team_ids: Deprecated; ignored (team1 is Radiant)
            dire_team_ids: Deprecated; ignored (team2 is Dire)
            dotabuff_match_id: Optional external match ID
            notes: Optional match notes

        Returns:
            Match ID
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Insert match record (team1=Radiant, team2=Dire)
            cursor.execute(
                """
                INSERT INTO matches (team1_players, team2_players, winning_team,
                                    dotabuff_match_id, notes, lobby_type)
                VALUES (?, ?, ?, ?, ?, ?)
            """,
                (
                    json.dumps(team1_ids),
                    json.dumps(team2_ids),
                    winning_team,
                    dotabuff_match_id,
                    notes,
                    lobby_type,
                ),
            )

            match_id = cursor.lastrowid

            # Insert participants with side (team1=radiant, team2=dire)
            team1_won = winning_team == 1

            for player_id in team1_ids:
                cursor.execute(
                    """
                    INSERT INTO match_participants (match_id, discord_id, team_number, won, side)
                    VALUES (?, ?, 1, ?, ?)
                """,
                    (match_id, player_id, team1_won, "radiant"),
                )

            for player_id in team2_ids:
                cursor.execute(
                    """
                    INSERT INTO match_participants (match_id, discord_id, team_number, won, side)
                    VALUES (?, ?, 2, ?, ?)
                """,
                    (match_id, player_id, not team1_won, "dire"),
                )

            return match_id

    def add_rating_history(
        self,
        discord_id: int,
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
    ) -> None:
        """Record a rating change in history."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO rating_history (
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
                    match_id
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
                (
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
                    match_id,
                ),
            )

    def _normalize_guild_id(self, guild_id: int | None) -> int:
        return guild_id if guild_id is not None else 0

    def save_pending_match(self, guild_id: int | None, payload: dict) -> None:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO pending_matches (guild_id, payload)
                VALUES (?, ?)
                ON CONFLICT(guild_id) DO UPDATE SET
                    payload = excluded.payload,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (normalized, json.dumps(payload)),
            )

    def get_pending_match(self, guild_id: int | None) -> dict | None:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT payload FROM pending_matches WHERE guild_id = ?", (normalized,))
            row = cursor.fetchone()
            if not row:
                return None
            return json.loads(row["payload"])

    def clear_pending_match(self, guild_id: int | None) -> None:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM pending_matches WHERE guild_id = ?", (normalized,))

    def consume_pending_match(self, guild_id: int | None) -> dict | None:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT payload FROM pending_matches WHERE guild_id = ?", (normalized,))
            row = cursor.fetchone()
            if not row:
                return None
            cursor.execute("DELETE FROM pending_matches WHERE guild_id = ?", (normalized,))
            return json.loads(row["payload"])

    def get_match(self, match_id: int) -> dict | None:
        """Get match by ID."""
        with self.connection() as conn:
            cursor = conn.cursor()
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
            }

    def get_player_matches(self, discord_id: int, limit: int = 10) -> list[dict]:
        """Get recent matches for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT m.*, mp.team_number, mp.won, mp.side
                FROM matches m
                JOIN match_participants mp ON m.match_id = mp.match_id
                WHERE mp.discord_id = ?
                ORDER BY m.match_date DESC
                LIMIT ?
            """,
                (discord_id, limit),
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

    def get_rating_history(self, discord_id: int, limit: int = 20) -> list[dict]:
        """Get rating history for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM rating_history
                WHERE discord_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
            """,
                (discord_id, limit),
            )

            rows = cursor.fetchall()
            return [
                {
                    "rating": row["rating"],
                    "match_id": row["match_id"],
                    "timestamp": row["timestamp"],
                }
                for row in rows
            ]

    def get_player_rating_history_detailed(self, discord_id: int, limit: int = 50) -> list[dict]:
        """Get detailed rating history for a player including prediction data."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT rh.*, m.lobby_type
                FROM rating_history rh
                LEFT JOIN matches m ON rh.match_id = m.match_id
                WHERE rh.discord_id = ?
                ORDER BY rh.timestamp DESC
                LIMIT ?
            """,
                (discord_id, limit),
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

    def get_recent_rating_history(self, limit: int = 200) -> list[dict]:
        """Get recent rating history entries for all players."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT *
                FROM rating_history
                ORDER BY timestamp DESC
                LIMIT ?
            """,
                (limit,),
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

    def delete_all_matches(self) -> int:
        """
        Delete all matches (for testing).

        Returns:
            Number of matches deleted
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute("SELECT COUNT(*) FROM matches")
            count = cursor.fetchone()[0]

            cursor.execute("DELETE FROM match_predictions")
            cursor.execute("DELETE FROM matches")
            cursor.execute("DELETE FROM match_participants")
            cursor.execute("DELETE FROM rating_history")

            return count

    def get_match_count(self) -> int:
        """Get total match count."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM matches")
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

    def get_recent_match_predictions(self, limit: int = 200) -> list[dict]:
        """Get recent match predictions with outcomes."""
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
                ORDER BY m.match_date DESC
                LIMIT ?
            """,
                (limit,),
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

    def get_most_recent_match(self) -> dict | None:
        """Get the most recently recorded match."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM matches ORDER BY match_date DESC LIMIT 1")
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

    def get_matches_without_enrichment(self, limit: int = 10) -> list[dict]:
        """Get matches that don't have Valve enrichment data yet."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM matches
                WHERE valve_match_id IS NULL
                ORDER BY match_date DESC
                LIMIT ?
                """,
                (limit,),
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
                    lane_efficiency = ?
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
                    match_id,
                    discord_id,
                ),
            )

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

    def get_player_hero_stats(self, discord_id: int) -> dict:
        """
        Get hero statistics for a player from enriched matches.

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
                WHERE mp.discord_id = ? AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                ORDER BY m.match_date DESC
                LIMIT 1
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            last_hero_id = row["hero_id"] if row else None

            # Get hero frequency (games and wins per hero)
            cursor.execute(
                """
                SELECT mp.hero_id, COUNT(*) as games, SUM(CASE WHEN mp.won THEN 1 ELSE 0 END) as wins
                FROM match_participants mp
                WHERE mp.discord_id = ? AND mp.hero_id IS NOT NULL AND mp.hero_id > 0
                GROUP BY mp.hero_id
                ORDER BY games DESC
                LIMIT 5
                """,
                (discord_id,),
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

            # Clear participant-level stats
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
                    net_worth = NULL
                WHERE match_id = ?
                """,
                (match_id,),
            )

            return True

    def wipe_auto_discovered_enrichments(self) -> int:
        """
        Clear all enrichments that were auto-discovered.

        Returns count of matches wiped.
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Get match IDs that are auto-discovered
            cursor.execute("SELECT match_id FROM matches WHERE enrichment_source = 'auto'")
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
                WHERE enrichment_source = 'auto'
                """
            )

            # Clear participant stats for those matches
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
                    net_worth = NULL
                WHERE match_id IN ({placeholders})
                """,
                match_ids,
            )

            return len(match_ids)

    def get_auto_discovered_count(self) -> int:
        """Get count of auto-discovered enriched matches."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM matches WHERE enrichment_source = 'auto'")
            return cursor.fetchone()["count"]

    def get_biggest_upsets(self, limit: int = 5) -> list[dict]:
        """Get matches where the underdog won, sorted by upset magnitude."""
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
                WHERE m.winning_team IS NOT NULL
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
                (limit,),
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

    def get_player_performance_stats(self) -> list[dict]:
        """Get player expected vs actual win stats from rating history."""
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
                WHERE expected_team_win_prob IS NOT NULL
                GROUP BY discord_id
                HAVING COUNT(*) >= 5
                ORDER BY (SUM(CASE WHEN won = 1 THEN 1 ELSE 0 END) - SUM(expected_team_win_prob)) DESC
            """
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

    def get_lobby_type_stats(self) -> list[dict]:
        """
        Get server-wide rating swing statistics by lobby type.

        Returns list of dicts with:
        - lobby_type: 'shuffle' or 'draft'
        - avg_swing: average absolute rating change
        - games: number of rating changes recorded
        - actual_win_rate: percentage of games won
        - expected_win_rate: average expected win probability
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
                WHERE rh.rating_before IS NOT NULL
                GROUP BY m.lobby_type
                """
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

    def get_player_lobby_type_stats(self, discord_id: int) -> list[dict]:
        """
        Get individual player's rating swing statistics by lobby type.

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
                WHERE rh.rating_before IS NOT NULL AND rh.discord_id = ?
                GROUP BY m.lobby_type
                """,
                (discord_id,),
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
