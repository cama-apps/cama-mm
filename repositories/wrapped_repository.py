"""
Repository for Cama Wrapped yearly summary data access.
"""

import logging
from datetime import UTC

from repositories.base_repository import BaseRepository
from repositories.interfaces import IWrappedRepository

logger = logging.getLogger("cama_bot.repositories.wrapped")


class WrappedRepository(BaseRepository, IWrappedRepository):
    """
    Data access layer for wrapped generation tracking and stats queries.
    """

    def get_month_match_stats(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """
        Get match participation stats for a time period.

        Returns list of dicts with player stats aggregated from matches.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    mp.discord_id,
                    p.discord_username,
                    COUNT(DISTINCT m.match_id) as games_played,
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN mp.won = 0 THEN 1 ELSE 0 END) as losses,
                    AVG(mp.gpm) as avg_gpm,
                    AVG(mp.xpm) as avg_xpm,
                    AVG(CASE WHEN mp.deaths > 0
                        THEN (CAST(mp.kills AS REAL) + mp.assists) / mp.deaths
                        ELSE mp.kills + mp.assists END) as avg_kda,
                    SUM(mp.kills) as total_kills,
                    SUM(mp.deaths) as total_deaths,
                    SUM(mp.assists) as total_assists,
                    SUM(COALESCE(mp.obs_placed, 0) + COALESCE(mp.sen_placed, 0)) as total_wards,
                    SUM(COALESCE(mp.fantasy_points, 0)) as total_fantasy,
                    p.glicko_rating,
                    p.glicko_rd
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                JOIN players p ON mp.discord_id = p.discord_id AND p.guild_id = ?
                WHERE m.winning_team IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                  AND m.guild_id = ?
                GROUP BY mp.discord_id
                ORDER BY games_played DESC
                """,
                (guild_id, start_ts, end_ts, guild_id),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_hero_stats(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """
        Get hero pick stats for a time period.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    mp.hero_id,
                    COUNT(*) as picks,
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins,
                    AVG(mp.gpm) as avg_gpm,
                    AVG(CASE WHEN mp.deaths > 0
                        THEN (CAST(mp.kills AS REAL) + mp.assists) / mp.deaths
                        ELSE mp.kills + mp.assists END) as avg_kda
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE m.winning_team IS NOT NULL
                  AND mp.hero_id IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                  AND m.guild_id = ?
                GROUP BY mp.hero_id
                ORDER BY picks DESC
                """,
                (start_ts, end_ts, guild_id),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_player_heroes(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """
        Get per-player hero stats for a time period.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    mp.discord_id,
                    mp.hero_id,
                    COUNT(*) as picks,
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins,
                    SUM(COALESCE(mp.kills, 0)) as total_kills,
                    SUM(COALESCE(mp.deaths, 0)) as total_deaths,
                    SUM(COALESCE(mp.assists, 0)) as total_assists
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE m.winning_team IS NOT NULL
                  AND mp.hero_id IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                  AND m.guild_id = ?
                GROUP BY mp.discord_id, mp.hero_id
                ORDER BY mp.discord_id, picks DESC
                """,
                (start_ts, end_ts, guild_id),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_rating_changes(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """
        Get rating changes for players over a time period.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            # Get first and last rating in the period for each player
            cursor.execute(
                """
                WITH first_rating AS (
                    SELECT discord_id, rating_before as first_rating,
                           ROW_NUMBER() OVER (PARTITION BY discord_id ORDER BY timestamp ASC) as rn
                    FROM rating_history rh
                    WHERE rh.timestamp >= datetime(?, 'unixepoch')
                      AND rh.timestamp < datetime(?, 'unixepoch')
                      AND rh.guild_id = ?
                      AND rating_before IS NOT NULL
                ),
                last_rating AS (
                    SELECT discord_id, rating as last_rating,
                           ROW_NUMBER() OVER (PARTITION BY discord_id ORDER BY timestamp DESC) as rn
                    FROM rating_history rh
                    WHERE rh.timestamp >= datetime(?, 'unixepoch')
                      AND rh.timestamp < datetime(?, 'unixepoch')
                      AND rh.guild_id = ?
                      AND rating IS NOT NULL
                ),
                rating_variance AS (
                    SELECT discord_id,
                           AVG(rating) as avg_rating,
                           -- Calculate variance manually
                           AVG(rating * rating) - AVG(rating) * AVG(rating) as rating_variance
                    FROM rating_history rh
                    WHERE rh.timestamp >= datetime(?, 'unixepoch')
                      AND rh.timestamp < datetime(?, 'unixepoch')
                      AND rh.guild_id = ?
                      AND rating IS NOT NULL
                    GROUP BY discord_id
                )
                SELECT
                    f.discord_id,
                    p.discord_username,
                    f.first_rating,
                    l.last_rating,
                    (l.last_rating - f.first_rating) as rating_change,
                    rv.rating_variance
                FROM first_rating f
                JOIN last_rating l ON f.discord_id = l.discord_id AND l.rn = 1
                JOIN players p ON f.discord_id = p.discord_id AND p.guild_id = ?
                LEFT JOIN rating_variance rv ON f.discord_id = rv.discord_id
                WHERE f.rn = 1
                ORDER BY rating_change DESC
                """,
                (start_ts, end_ts, guild_id, start_ts, end_ts, guild_id, start_ts, end_ts, guild_id, guild_id),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_betting_stats(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """
        Get betting stats for players over a time period.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    b.discord_id,
                    p.discord_username,
                    COUNT(*) as total_bets,
                    SUM(b.amount * COALESCE(b.leverage, 1)) as total_wagered,
                    SUM(CASE WHEN b.payout IS NOT NULL THEN b.payout - b.amount * COALESCE(b.leverage, 1) ELSE -b.amount * COALESCE(b.leverage, 1) END) as net_pnl,
                    SUM(CASE WHEN b.payout IS NOT NULL AND b.payout > 0 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN b.payout IS NULL OR b.payout = 0 THEN 1 ELSE 0 END) as losses
                FROM bets b
                JOIN matches m ON b.match_id = m.match_id
                JOIN players p ON b.discord_id = p.discord_id AND p.guild_id = ?
                WHERE m.winning_team IS NOT NULL
                  AND b.guild_id = ?
                  AND b.bet_time >= ?
                  AND b.bet_time < ?
                GROUP BY b.discord_id
                ORDER BY total_wagered DESC
                """,
                (guild_id, guild_id, start_ts, end_ts),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_bankruptcy_count(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """
        Get bankruptcy counts for the period.
        Note: bankruptcy_state doesn't have per-event timestamps, so this
        counts players who declared bankruptcy and have last_bankruptcy_at in range.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    bs.discord_id,
                    p.discord_username,
                    bs.bankruptcy_count
                FROM bankruptcy_state bs
                JOIN players p ON bs.discord_id = p.discord_id AND p.guild_id = ?
                WHERE bs.last_bankruptcy_at >= ?
                  AND bs.last_bankruptcy_at < ?
                  AND bs.guild_id = ?
                ORDER BY bs.bankruptcy_count DESC
                """,
                (guild_id, start_ts, end_ts, guild_id),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_bets_against_player(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """
        Get count of bets placed against each player's team.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    mp.discord_id,
                    p.discord_username,
                    COUNT(b.bet_id) as bets_against
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                JOIN bets b ON b.match_id = m.match_id
                    AND b.discord_id != mp.discord_id
                    AND ((mp.side = 'radiant' AND b.team_bet_on = 'dire')
                         OR (mp.side = 'dire' AND b.team_bet_on = 'radiant'))
                JOIN players p ON mp.discord_id = p.discord_id AND p.guild_id = ?
                WHERE m.winning_team IS NOT NULL
                  AND b.guild_id = ?
                  AND b.bet_time >= ?
                  AND b.bet_time < ?
                GROUP BY mp.discord_id
                ORDER BY bets_against DESC
                """,
                (guild_id, guild_id, start_ts, end_ts),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_summary(self, guild_id: int, start_ts: int, end_ts: int) -> dict:
        """
        Get high-level summary stats for the month.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            # Get match stats
            cursor.execute(
                """
                SELECT
                    COUNT(DISTINCT m.match_id) as total_matches,
                    COUNT(DISTINCT mp.discord_id) as total_players,
                    COUNT(DISTINCT mp.hero_id) as unique_heroes
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE m.winning_team IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                  AND m.guild_id = ?
                """,
                (start_ts, end_ts, guild_id),
            )
            row = cursor.fetchone()
            result = dict(row) if row else {}

            # Get total JC wagered
            cursor.execute(
                """
                SELECT COALESCE(SUM(b.amount * COALESCE(b.leverage, 1)), 0) as total_wagered
                FROM bets b
                JOIN matches m ON b.match_id = m.match_id
                WHERE m.winning_team IS NOT NULL
                  AND b.bet_time >= ?
                  AND b.bet_time < ?
                  AND b.guild_id = ?
                """,
                (start_ts, end_ts, guild_id),
            )
            wager_row = cursor.fetchone()
            result["total_wagered"] = wager_row["total_wagered"] if wager_row else 0

            return result

    def get_player_year_matches(
        self, discord_id: int, guild_id: int | None, year: int, end_ts: int
    ) -> list[dict]:
        """
        Get per-match rows for a player from Jan 1 of year through end_ts.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            year: Year to start from (Jan 1)
            end_ts: End Unix timestamp

        Returns:
            List of dicts with per-match stats including enrichment_data
        """
        guild_id = self.normalize_guild_id(guild_id)
        # Jan 1 of year at 00:00 UTC
        from datetime import datetime
        start_ts = int(datetime(year, 1, 1, tzinfo=UTC).timestamp())
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    mp.match_id,
                    mp.discord_id,
                    mp.hero_id,
                    mp.kills,
                    mp.deaths,
                    mp.assists,
                    mp.last_hits,
                    mp.denies,
                    mp.gpm,
                    mp.xpm,
                    mp.hero_damage,
                    mp.tower_damage,
                    mp.hero_healing,
                    mp.obs_placed,
                    mp.sen_placed,
                    mp.stuns,
                    mp.towers_killed,
                    mp.won,
                    m.match_date,
                    m.duration_seconds,
                    m.valve_match_id,
                    m.enrichment_data,
                    mp.team_number,
                    mp.side,
                    m.radiant_score,
                    m.dire_score
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id = ?
                  AND m.guild_id = ?
                  AND m.winning_team IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                ORDER BY m.match_date ASC
                """,
                (discord_id, guild_id, start_ts, end_ts),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_month_player_match_details(
        self, discord_id: int, guild_id: int, start_ts: int, end_ts: int
    ) -> dict | None:
        """Get a player's match stats (games, wins, losses) for a time period.

        Returns dict with 'games_played', 'wins', 'losses' or None if no games.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    COUNT(DISTINCT m.match_id) as games_played,
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN mp.won = 0 THEN 1 ELSE 0 END) as losses
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id = ?
                  AND m.guild_id = ?
                  AND m.winning_team IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                """,
                (discord_id, guild_id, start_ts, end_ts),
            )
            row = cursor.fetchone()
            if not row or row["games_played"] == 0:
                return None
            return {
                "games_played": row["games_played"],
                "wins": row["wins"] or 0,
                "losses": row["losses"] or 0,
            }
