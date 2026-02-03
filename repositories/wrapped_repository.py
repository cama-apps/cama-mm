"""
Repository for Cama Wrapped monthly summary data access.
"""

import json
import logging
import time
from typing import Any

from repositories.base_repository import BaseRepository

logger = logging.getLogger("cama_bot.repositories.wrapped")


class WrappedRepository(BaseRepository):
    """
    Data access layer for wrapped generation tracking and stats queries.
    """

    def get_wrapped(self, guild_id: int, year_month: str) -> dict | None:
        """
        Get existing wrapped generation record for a guild/month.

        Args:
            guild_id: Discord guild ID
            year_month: Month in "YYYY-MM" format

        Returns:
            Wrapped record dict or None if not generated
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT id, guild_id, year_month, channel_id, message_id,
                       generated_at, generated_by, generation_type, stats_json
                FROM wrapped_generation
                WHERE guild_id = ? AND year_month = ?
                """,
                (guild_id, year_month),
            )
            row = cursor.fetchone()
            if row:
                return dict(row)
            return None

    def get_last_generation(self, guild_id: int) -> dict | None:
        """
        Get the most recent wrapped generation for a guild.

        Returns:
            Most recent wrapped record dict or None
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT id, guild_id, year_month, generated_at, generation_type
                FROM wrapped_generation
                WHERE guild_id = ?
                ORDER BY generated_at DESC
                LIMIT 1
                """,
                (guild_id,),
            )
            row = cursor.fetchone()
            if row:
                return dict(row)
            return None

    def save_wrapped(
        self,
        guild_id: int,
        year_month: str,
        stats: dict,
        channel_id: int | None = None,
        message_id: int | None = None,
        generated_by: int | None = None,
        generation_type: str = "auto",
    ) -> int:
        """
        Save wrapped generation record.

        Args:
            guild_id: Discord guild ID
            year_month: Month in "YYYY-MM" format
            stats: Stats dictionary to cache
            channel_id: Channel where posted
            message_id: Message ID of posted wrapped
            generated_by: Discord ID of user who triggered (None for auto)
            generation_type: 'auto' or 'manual'

        Returns:
            ID of the saved record
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.cursor() as cursor:
            cursor.execute(
                """
                INSERT INTO wrapped_generation
                    (guild_id, year_month, channel_id, message_id, generated_at,
                     generated_by, generation_type, stats_json)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(guild_id, year_month) DO UPDATE SET
                    channel_id = excluded.channel_id,
                    message_id = excluded.message_id,
                    generated_at = excluded.generated_at,
                    generated_by = excluded.generated_by,
                    generation_type = excluded.generation_type,
                    stats_json = excluded.stats_json
                """,
                (
                    guild_id,
                    year_month,
                    channel_id,
                    message_id,
                    int(time.time()),
                    generated_by,
                    generation_type,
                    json.dumps(stats),
                ),
            )
            return cursor.lastrowid or 0

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
                JOIN players p ON mp.discord_id = p.discord_id
                WHERE m.winning_team IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                GROUP BY mp.discord_id
                ORDER BY games_played DESC
                """,
                (start_ts, end_ts),
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
                GROUP BY mp.hero_id
                ORDER BY picks DESC
                """,
                (start_ts, end_ts),
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
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE m.winning_team IS NOT NULL
                  AND mp.hero_id IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                GROUP BY mp.discord_id, mp.hero_id
                ORDER BY mp.discord_id, picks DESC
                """,
                (start_ts, end_ts),
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
                    FROM rating_history
                    WHERE timestamp >= datetime(?, 'unixepoch')
                      AND timestamp < datetime(?, 'unixepoch')
                      AND rating_before IS NOT NULL
                ),
                last_rating AS (
                    SELECT discord_id, rating as last_rating,
                           ROW_NUMBER() OVER (PARTITION BY discord_id ORDER BY timestamp DESC) as rn
                    FROM rating_history
                    WHERE timestamp >= datetime(?, 'unixepoch')
                      AND timestamp < datetime(?, 'unixepoch')
                      AND rating IS NOT NULL
                ),
                rating_variance AS (
                    SELECT discord_id,
                           AVG(rating) as avg_rating,
                           -- Calculate variance manually
                           AVG(rating * rating) - AVG(rating) * AVG(rating) as rating_variance
                    FROM rating_history
                    WHERE timestamp >= datetime(?, 'unixepoch')
                      AND timestamp < datetime(?, 'unixepoch')
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
                JOIN players p ON f.discord_id = p.discord_id
                LEFT JOIN rating_variance rv ON f.discord_id = rv.discord_id
                WHERE f.rn = 1
                ORDER BY rating_change DESC
                """,
                (start_ts, end_ts, start_ts, end_ts, start_ts, end_ts),
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
                JOIN players p ON b.discord_id = p.discord_id
                WHERE m.winning_team IS NOT NULL
                  AND b.guild_id = ?
                  AND b.bet_time >= ?
                  AND b.bet_time < ?
                GROUP BY b.discord_id
                ORDER BY total_wagered DESC
                """,
                (guild_id, start_ts, end_ts),
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
        with self.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    bs.discord_id,
                    p.discord_username,
                    bs.bankruptcy_count
                FROM bankruptcy_state bs
                JOIN players p ON bs.discord_id = p.discord_id
                WHERE bs.last_bankruptcy_at >= ?
                  AND bs.last_bankruptcy_at < ?
                ORDER BY bs.bankruptcy_count DESC
                """,
                (start_ts, end_ts),
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
                JOIN players p ON mp.discord_id = p.discord_id
                WHERE m.winning_team IS NOT NULL
                  AND b.guild_id = ?
                  AND b.bet_time >= ?
                  AND b.bet_time < ?
                GROUP BY mp.discord_id
                ORDER BY bets_against DESC
                """,
                (guild_id, start_ts, end_ts),
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
                """,
                (start_ts, end_ts),
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
                """,
                (start_ts, end_ts),
            )
            wager_row = cursor.fetchone()
            result["total_wagered"] = wager_row["total_wagered"] if wager_row else 0

            return result
