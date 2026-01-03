"""
Repository for match data access.
"""

import json
import logging
from typing import Dict, List, Optional

from repositories.base_repository import BaseRepository
from repositories.interfaces import IMatchRepository

logger = logging.getLogger('cama_bot.repositories.match')


class MatchRepository(BaseRepository, IMatchRepository):
    """
    Handles all match-related database operations.
    
    Responsibilities:
    - Match recording
    - Match participant tracking
    - Rating history
    """
    
    def record_match(self,
                     team1_ids: List[int],
                     team2_ids: List[int],
                     winning_team: int,
                     radiant_team_ids: Optional[List[int]] = None,
                     dire_team_ids: Optional[List[int]] = None,
                     dotabuff_match_id: Optional[str] = None,
                     notes: Optional[str] = None) -> int:
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
            cursor.execute("""
                INSERT INTO matches (team1_players, team2_players, winning_team, 
                                    dotabuff_match_id, notes)
                VALUES (?, ?, ?, ?, ?)
            """, (json.dumps(team1_ids), json.dumps(team2_ids), winning_team,
                  dotabuff_match_id, notes))
            
            match_id = cursor.lastrowid
            
            # Insert participants with side (team1=radiant, team2=dire)
            team1_won = (winning_team == 1)

            for player_id in team1_ids:
                cursor.execute("""
                    INSERT INTO match_participants (match_id, discord_id, team_number, won, side)
                    VALUES (?, ?, 1, ?, ?)
                """, (match_id, player_id, team1_won, "radiant"))

            for player_id in team2_ids:
                cursor.execute("""
                    INSERT INTO match_participants (match_id, discord_id, team_number, won, side)
                    VALUES (?, ?, 2, ?, ?)
                """, (match_id, player_id, not team1_won, "dire"))
            
            return match_id
    
    def add_rating_history(self,
                          discord_id: int,
                          rating: float,
                          match_id: Optional[int] = None) -> None:
        """Record a rating change in history."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("""
                INSERT INTO rating_history (discord_id, rating, match_id)
                VALUES (?, ?, ?)
            """, (discord_id, rating, match_id))

    def _normalize_guild_id(self, guild_id: Optional[int]) -> int:
        return guild_id if guild_id is not None else 0

    def save_pending_match(self, guild_id: Optional[int], payload: Dict) -> None:
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

    def get_pending_match(self, guild_id: Optional[int]) -> Optional[Dict]:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT payload FROM pending_matches WHERE guild_id = ?", (normalized,))
            row = cursor.fetchone()
            if not row:
                return None
            return json.loads(row["payload"])

    def clear_pending_match(self, guild_id: Optional[int]) -> None:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM pending_matches WHERE guild_id = ?", (normalized,))

    def consume_pending_match(self, guild_id: Optional[int]) -> Optional[Dict]:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT payload FROM pending_matches WHERE guild_id = ?", (normalized,))
            row = cursor.fetchone()
            if not row:
                return None
            cursor.execute("DELETE FROM pending_matches WHERE guild_id = ?", (normalized,))
            return json.loads(row["payload"])
    
    def get_match(self, match_id: int) -> Optional[dict]:
        """Get match by ID."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM matches WHERE match_id = ?", (match_id,))
            row = cursor.fetchone()
            
            if not row:
                return None
            
            return {
                'match_id': row['match_id'],
                'team1_players': json.loads(row['team1_players']),
                'team2_players': json.loads(row['team2_players']),
                'winning_team': row['winning_team'],
                'match_date': row['match_date'],
                'dotabuff_match_id': row['dotabuff_match_id'],
                'notes': row['notes']
            }
    
    def get_player_matches(self, discord_id: int, limit: int = 10) -> List[dict]:
        """Get recent matches for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("""
                SELECT m.*, mp.team_number, mp.won, mp.side
                FROM matches m
                JOIN match_participants mp ON m.match_id = mp.match_id
                WHERE mp.discord_id = ?
                ORDER BY m.match_date DESC
                LIMIT ?
            """, (discord_id, limit))
            
            rows = cursor.fetchall()
            return [{
                'match_id': row['match_id'],
                'team1_players': json.loads(row['team1_players']),
                'team2_players': json.loads(row['team2_players']),
                'winning_team': row['winning_team'],
                'match_date': row['match_date'],
                'player_team': row['team_number'],
                'player_won': bool(row['won']),
                'side': row['side']
            } for row in rows]
    
    def get_rating_history(self, discord_id: int, limit: int = 20) -> List[dict]:
        """Get rating history for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("""
                SELECT * FROM rating_history
                WHERE discord_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
            """, (discord_id, limit))
            
            rows = cursor.fetchall()
            return [{
                'rating': row['rating'],
                'match_id': row['match_id'],
                'timestamp': row['timestamp']
            } for row in rows]
    
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

            cursor.execute("DELETE FROM matches")
            cursor.execute("DELETE FROM match_participants")
            cursor.execute("DELETE FROM rating_history")

            return count

    def get_most_recent_match(self) -> Optional[dict]:
        """Get the most recently recorded match."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM matches ORDER BY match_date DESC LIMIT 1"
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

    def get_matches_without_enrichment(self, limit: int = 10) -> List[dict]:
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
        enrichment_data: Optional[str] = None,
        enrichment_source: Optional[str] = None,
        enrichment_confidence: Optional[float] = None,
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
    ) -> None:
        """Update a match participant with enriched stats from Valve API."""
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
                    net_worth = ?
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
                    match_id,
                    discord_id,
                ),
            )

    def get_match_participants(self, match_id: int) -> List[dict]:
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
            cursor.execute(
                "SELECT match_id FROM matches WHERE enrichment_source = 'auto'"
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
            cursor.execute(
                "SELECT COUNT(*) as count FROM matches WHERE enrichment_source = 'auto'"
            )
            return cursor.fetchone()["count"]

