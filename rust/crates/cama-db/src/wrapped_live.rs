//! Complete read-only projections for the live `/wrapped` story.
//!
//! These queries mirror `repositories.wrapped_repository.WrappedRepository`
//! against the Rust-migrated schema.  The command is intentionally read-only:
//! generation state is held by Discord for ten minutes and no Wrapped query
//! mutates match, economy, or rating history.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cama_domain::wrapped_distinct::{DistinctMatch, DistinctPlayer};
use chrono::{DateTime, NaiveDateTime};
use rusqlite::{OptionalExtension, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WrappedServerSummary {
    pub total_matches: i64,
    pub total_players: i64,
    pub unique_heroes: i64,
    pub total_wagered: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedPlayerStats {
    pub discord_id: i64,
    pub discord_username: String,
    pub games_played: i64,
    pub wins: i64,
    pub losses: i64,
    pub avg_gpm: f64,
    pub avg_xpm: f64,
    pub avg_kda: f64,
    pub total_kills: i64,
    pub total_deaths: i64,
    pub total_assists: i64,
    pub total_wards: i64,
    pub total_fantasy: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedHeroStats {
    pub hero_id: i64,
    pub picks: i64,
    pub wins: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedPlayerHeroStats {
    pub discord_id: i64,
    pub hero_id: i64,
    pub picks: i64,
    pub wins: i64,
    pub total_kills: i64,
    pub total_deaths: i64,
    pub total_assists: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedRatingChange {
    pub discord_id: i64,
    pub discord_username: String,
    pub first_rating: f64,
    pub last_rating: f64,
    pub rating_change: f64,
    pub rating_variance: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedBettingStats {
    pub discord_id: i64,
    pub discord_username: String,
    pub total_bets: i64,
    pub total_wagered: i64,
    pub net_pnl: i64,
    pub wins: i64,
    pub losses: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedBetsAgainst {
    pub discord_id: i64,
    pub discord_username: String,
    pub bets_against: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedBankruptcy {
    pub discord_id: i64,
    pub discord_username: String,
    pub bankruptcy_count: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedYearMatch {
    pub match_id: i64,
    pub discord_id: i64,
    pub hero_id: Option<i64>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub last_hits: Option<i64>,
    pub denies: Option<i64>,
    pub gpm: Option<i64>,
    pub xpm: Option<i64>,
    pub hero_damage: Option<i64>,
    pub tower_damage: Option<i64>,
    pub hero_healing: Option<i64>,
    pub obs_placed: Option<i64>,
    pub sen_placed: Option<i64>,
    pub stuns: Option<f64>,
    pub towers_killed: Option<i64>,
    pub won: Option<bool>,
    pub match_date: Option<String>,
    pub duration_seconds: Option<i64>,
    pub valve_match_id: Option<i64>,
    pub side: Option<String>,
    pub radiant_score: Option<i64>,
    pub dire_score: Option<i64>,
    pub enrichment_present: bool,
    pub actions_per_min: Option<f64>,
    pub courier_kills: Option<i64>,
    pub pings: Option<i64>,
    pub rapier_count: i64,
    pub lane_role: Option<i32>,
    pub comeback: Option<f64>,
    pub throw: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedRatingPoint {
    pub rating: Option<f64>,
    pub rating_before: Option<f64>,
    pub os_mu_after: Option<f64>,
    pub won: Option<bool>,
    pub timestamp: String,
}

#[derive(Debug, Error)]
pub enum WrappedLiveRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// `matches.match_date` is stored as RFC3339 or `YYYY-MM-DD HH:MM:SS` (UTC).
fn parse_match_date(raw: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(raw)
        .map(|date| date.timestamp())
        .or_else(|_| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
                .map(|date| date.and_utc().timestamp())
        })
        .ok()
}

/// `{"blink":1,"magic_wand":2}` sorted by key; malformed text is empty.
fn parse_item_counts(raw: &str) -> Vec<(String, i64)> {
    serde_json::from_str::<BTreeMap<String, i64>>(raw)
        .map(|counts| counts.into_iter().collect())
        .unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct WrappedLiveRepository {
    path: PathBuf,
}

impl WrappedLiveRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn server_summary(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<WrappedServerSummary, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let (total_matches, total_players, unique_heroes) = connection.query_row(
            "SELECT COUNT(DISTINCT m.match_id),
                    COUNT(DISTINCT mp.discord_id),
                    COUNT(DISTINCT mp.hero_id)
               FROM matches AS m
               JOIN match_participants AS mp ON mp.match_id = m.match_id
              WHERE m.winning_team IS NOT NULL
                AND m.guild_id = ?1
                AND m.match_date >= datetime(?2, 'unixepoch')
                AND m.match_date < datetime(?3, 'unixepoch')",
            params![guild_id, start_ts, end_ts],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let total_wagered = connection.query_row(
            "SELECT COALESCE(SUM(b.amount * COALESCE(b.leverage, 1)), 0)
               FROM bets AS b
               JOIN matches AS m ON m.match_id = b.match_id
              WHERE m.winning_team IS NOT NULL
                AND b.guild_id = ?1
                AND b.bet_time >= ?2
                AND b.bet_time < ?3",
            params![guild_id, start_ts, end_ts],
            |row| row.get(0),
        )?;
        Ok(WrappedServerSummary {
            total_matches,
            total_players,
            unique_heroes,
            total_wagered,
        })
    }

    pub fn player_stats(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedPlayerStats>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT mp.discord_id, p.discord_username,
                    COUNT(DISTINCT m.match_id),
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN mp.won = 0 THEN 1 ELSE 0 END),
                    COALESCE(AVG(mp.gpm), 0), COALESCE(AVG(mp.xpm), 0),
                    COALESCE(AVG(CASE WHEN mp.deaths > 0
                         THEN (CAST(mp.kills AS REAL) + mp.assists) / mp.deaths
                         ELSE mp.kills + mp.assists END), 0),
                    COALESCE(SUM(mp.kills), 0), COALESCE(SUM(mp.deaths), 0),
                    COALESCE(SUM(mp.assists), 0),
                    COALESCE(SUM(COALESCE(mp.obs_placed, 0) + COALESCE(mp.sen_placed, 0)), 0),
                    COALESCE(SUM(mp.fantasy_points), 0)
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
               JOIN players AS p ON p.discord_id = mp.discord_id AND p.guild_id = ?1
              WHERE m.winning_team IS NOT NULL
                AND m.guild_id = ?1
                AND m.match_date >= datetime(?2, 'unixepoch')
                AND m.match_date < datetime(?3, 'unixepoch')
           GROUP BY mp.discord_id
           ORDER BY COUNT(DISTINCT m.match_id) DESC",
        )?;
        statement
            .query_map(params![guild_id, start_ts, end_ts], |row| {
                Ok(WrappedPlayerStats {
                    discord_id: row.get(0)?,
                    discord_username: row.get(1)?,
                    games_played: row.get(2)?,
                    wins: row.get(3)?,
                    losses: row.get(4)?,
                    avg_gpm: row.get(5)?,
                    avg_xpm: row.get(6)?,
                    avg_kda: row.get(7)?,
                    total_kills: row.get(8)?,
                    total_deaths: row.get(9)?,
                    total_assists: row.get(10)?,
                    total_wards: row.get(11)?,
                    total_fantasy: row.get(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn hero_stats(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedHeroStats>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT mp.hero_id, COUNT(*),
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END)
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
              WHERE m.winning_team IS NOT NULL AND mp.hero_id IS NOT NULL
                AND m.guild_id = ?1
                AND m.match_date >= datetime(?2, 'unixepoch')
                AND m.match_date < datetime(?3, 'unixepoch')
           GROUP BY mp.hero_id ORDER BY COUNT(*) DESC",
        )?;
        statement
            .query_map(params![guild_id, start_ts, end_ts], |row| {
                Ok(WrappedHeroStats {
                    hero_id: row.get(0)?,
                    picks: row.get(1)?,
                    wins: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn player_hero_stats(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedPlayerHeroStats>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT mp.discord_id, mp.hero_id, COUNT(*),
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END),
                    COALESCE(SUM(mp.kills), 0), COALESCE(SUM(mp.deaths), 0),
                    COALESCE(SUM(mp.assists), 0)
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
              WHERE m.winning_team IS NOT NULL AND mp.hero_id IS NOT NULL
                AND m.guild_id = ?1
                AND m.match_date >= datetime(?2, 'unixepoch')
                AND m.match_date < datetime(?3, 'unixepoch')
           GROUP BY mp.discord_id, mp.hero_id
           ORDER BY mp.discord_id, COUNT(*) DESC",
        )?;
        statement
            .query_map(params![guild_id, start_ts, end_ts], |row| {
                Ok(WrappedPlayerHeroStats {
                    discord_id: row.get(0)?,
                    hero_id: row.get(1)?,
                    picks: row.get(2)?,
                    wins: row.get(3)?,
                    total_kills: row.get(4)?,
                    total_deaths: row.get(5)?,
                    total_assists: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn rating_changes(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedRatingChange>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "WITH first_rating AS (
                 SELECT discord_id, rating_before AS first_rating,
                        ROW_NUMBER() OVER (
                            PARTITION BY discord_id ORDER BY timestamp ASC
                        ) AS rn
                   FROM rating_history
                  WHERE timestamp >= datetime(?1, 'unixepoch')
                    AND timestamp < datetime(?2, 'unixepoch')
                    AND guild_id = ?3 AND rating_before IS NOT NULL
             ), last_rating AS (
                 SELECT discord_id, rating AS last_rating,
                        ROW_NUMBER() OVER (
                            PARTITION BY discord_id ORDER BY timestamp DESC
                        ) AS rn
                   FROM rating_history
                  WHERE timestamp >= datetime(?1, 'unixepoch')
                    AND timestamp < datetime(?2, 'unixepoch')
                    AND guild_id = ?3 AND rating IS NOT NULL
             ), rating_variance AS (
                 SELECT discord_id,
                        AVG(rating * rating) - AVG(rating) * AVG(rating)
                            AS rating_variance
                   FROM rating_history
                  WHERE timestamp >= datetime(?1, 'unixepoch')
                    AND timestamp < datetime(?2, 'unixepoch')
                    AND guild_id = ?3 AND rating IS NOT NULL
               GROUP BY discord_id
             )
             SELECT first.discord_id, player.discord_username,
                    first.first_rating, last.last_rating,
                    last.last_rating - first.first_rating, variance.rating_variance
               FROM first_rating AS first
               JOIN last_rating AS last
                 ON last.discord_id = first.discord_id AND last.rn = 1
               JOIN players AS player
                 ON player.discord_id = first.discord_id AND player.guild_id = ?3
          LEFT JOIN rating_variance AS variance
                 ON variance.discord_id = first.discord_id
              WHERE first.rn = 1
           ORDER BY 5 DESC",
        )?;
        statement
            .query_map(params![start_ts, end_ts, guild_id], |row| {
                Ok(WrappedRatingChange {
                    discord_id: row.get(0)?,
                    discord_username: row.get(1)?,
                    first_rating: row.get(2)?,
                    last_rating: row.get(3)?,
                    rating_change: row.get(4)?,
                    rating_variance: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn betting_stats(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedBettingStats>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(concat!(
            "WITH settled AS (",
            include_str!("gambling_stats/settled_dota_bets.sql"),
            ")
             SELECT b.discord_id, p.discord_username, COUNT(*),
                    SUM(b.effective_bet), SUM(b.net_profit), SUM(b.won), SUM(1 - b.won)
               FROM settled AS b
               JOIN players AS p ON p.discord_id = b.discord_id AND p.guild_id = b.guild_id
              WHERE b.guild_id = ?1
                AND b.bet_time >= ?2 AND b.bet_time < ?3
           GROUP BY b.discord_id ORDER BY 4 DESC, b.discord_id"
        ))?;
        statement
            .query_map(params![guild_id, start_ts, end_ts], |row| {
                Ok(WrappedBettingStats {
                    discord_id: row.get(0)?,
                    discord_username: row.get(1)?,
                    total_bets: row.get(2)?,
                    total_wagered: row.get(3)?,
                    net_pnl: row.get(4)?,
                    wins: row.get(5)?,
                    losses: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn bets_against(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedBetsAgainst>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT mp.discord_id, p.discord_username, COUNT(b.bet_id)
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
               JOIN bets AS b ON b.match_id = m.match_id
                 AND b.discord_id != mp.discord_id
                 AND ((mp.side = 'radiant' AND b.team_bet_on = 'dire')
                   OR (mp.side = 'dire' AND b.team_bet_on = 'radiant'))
               JOIN players AS p ON p.discord_id = mp.discord_id AND p.guild_id = ?1
              WHERE m.winning_team IS NOT NULL AND b.guild_id = ?1
                AND b.bet_time >= ?2 AND b.bet_time < ?3
           GROUP BY mp.discord_id ORDER BY COUNT(b.bet_id) DESC",
        )?;
        statement
            .query_map(params![guild_id, start_ts, end_ts], |row| {
                Ok(WrappedBetsAgainst {
                    discord_id: row.get(0)?,
                    discord_username: row.get(1)?,
                    bets_against: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn bankruptcies(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedBankruptcy>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            // One declaration writes exactly one ledger row (the balance
            // reset trigger), so rows in the window are this year's events.
            // `related_id` holds the player, not an event id.
            "SELECT le.account_id, p.discord_username, COUNT(*) AS events
               FROM economy_ledger_entries AS le
               JOIN players AS p ON p.discord_id = le.account_id AND p.guild_id = ?1
              WHERE le.guild_id = ?1 AND le.related_type = 'bankruptcy'
                AND le.created_at >= ?2 AND le.created_at < ?3
           GROUP BY le.account_id
           ORDER BY events DESC, le.account_id ASC",
        )?;
        statement
            .query_map(params![guild_id, start_ts, end_ts], |row| {
                Ok(WrappedBankruptcy {
                    discord_id: row.get(0)?,
                    discord_username: row.get(1)?,
                    bankruptcy_count: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn player_year_matches(
        &self,
        discord_id: i64,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedYearMatch>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT mp.match_id, mp.discord_id, mp.hero_id, mp.kills, mp.deaths,
                    mp.assists, mp.last_hits, mp.denies, mp.gpm, mp.xpm,
                    mp.hero_damage, mp.tower_damage, mp.hero_healing,
                    mp.obs_placed, mp.sen_placed, mp.stuns, mp.towers_killed,
                    mp.won, m.match_date, m.duration_seconds, m.valve_match_id,
                    mp.side, m.radiant_score, m.dire_score, wf.match_id,
                    wf.actions_per_min, wf.courier_kills, wf.pings,
                    COALESCE(wf.rapier_count, 0), wf.lane_role, wf.comeback, wf.throw
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
          LEFT JOIN wrapped_enrichment_facts AS wf
                 ON wf.guild_id = m.guild_id AND wf.match_id = mp.match_id
                AND wf.discord_id = mp.discord_id
              WHERE mp.discord_id = ?1 AND m.guild_id = ?2
                AND m.winning_team IS NOT NULL
                AND m.match_date >= datetime(?3, 'unixepoch')
                AND m.match_date < datetime(?4, 'unixepoch')
           ORDER BY m.match_date ASC",
        )?;
        statement
            .query_map(params![discord_id, guild_id, start_ts, end_ts], |row| {
                Ok(WrappedYearMatch {
                    match_id: row.get(0)?,
                    discord_id: row.get(1)?,
                    hero_id: row.get(2)?,
                    kills: row.get(3)?,
                    deaths: row.get(4)?,
                    assists: row.get(5)?,
                    last_hits: row.get(6)?,
                    denies: row.get(7)?,
                    gpm: row.get(8)?,
                    xpm: row.get(9)?,
                    hero_damage: row.get(10)?,
                    tower_damage: row.get(11)?,
                    hero_healing: row.get(12)?,
                    obs_placed: row.get(13)?,
                    sen_placed: row.get(14)?,
                    stuns: row.get(15)?,
                    towers_killed: row.get(16)?,
                    won: row.get(17)?,
                    match_date: row.get(18)?,
                    duration_seconds: row.get(19)?,
                    valve_match_id: row.get(20)?,
                    side: row.get(21)?,
                    radiant_score: row.get(22)?,
                    dire_score: row.get(23)?,
                    enrichment_present: row.get::<_, Option<i64>>(24)?.is_some(),
                    actions_per_min: row.get(25)?,
                    courier_kills: row.get(26)?,
                    pings: row.get(27)?,
                    rapier_count: row.get(28)?,
                    lane_role: row.get(29)?,
                    comeback: row.get(30)?,
                    throw: row.get(31)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn rating_history(
        &self,
        discord_id: i64,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<WrappedRatingPoint>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT rating, rating_before, os_mu_after, won, CAST(timestamp AS TEXT)
               FROM rating_history
              WHERE discord_id = ?1 AND guild_id = ?2
                AND timestamp >= datetime(?3, 'unixepoch')
                AND timestamp < datetime(?4, 'unixepoch')
           ORDER BY timestamp DESC, id DESC",
        )?;
        statement
            .query_map(params![discord_id, guild_id, start_ts, end_ts], |row| {
                Ok(WrappedRatingPoint {
                    rating: row.get(0)?,
                    rating_before: row.get(1)?,
                    os_mu_after: row.get(2)?,
                    won: row.get(3)?,
                    timestamp: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// One row per participant of every recorded guild match in the window,
    /// for registered players only, plus those players' time zones.
    pub fn guild_year_rows(
        &self,
        guild_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<(Vec<DistinctMatch>, Vec<DistinctPlayer>), WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        // Correction replays can leave several rating rows for one player and
        // match; the latest id is the one that stands.
        let mut statement = connection.prepare(
            "SELECT mp.match_id, mp.discord_id, mp.side, mp.won, m.match_date,
                    m.duration_seconds, mp.hero_id, mp.kills, mp.deaths, mp.assists,
                    mp.last_hits, mp.gpm, mp.hero_damage, mp.tower_damage,
                    mp.hero_healing,
                    CASE WHEN mp.obs_placed IS NULL AND mp.sen_placed IS NULL THEN NULL
                         ELSE COALESCE(mp.obs_placed, 0) + COALESCE(mp.sen_placed, 0) END,
                    mp.camps_stacked, mp.rune_pickups, mp.stuns,
                    wf.match_id IS NOT NULL, wf.courier_kills, wf.pings,
                    wf.actions_per_min, wf.rapier_count, wf.buybacks, wf.item_counts,
                    wf.comeback, wf.throw,
                    rh.expected_team_win_prob, rh.rating - rh.rating_before,
                    p.timezone
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
               JOIN players AS p ON p.discord_id = mp.discord_id AND p.guild_id = ?1
          LEFT JOIN wrapped_enrichment_facts AS wf
                 ON wf.guild_id = m.guild_id AND wf.match_id = mp.match_id
                AND wf.discord_id = mp.discord_id
          LEFT JOIN rating_history AS rh
                 ON rh.id = (SELECT MAX(r2.id) FROM rating_history AS r2
                              WHERE r2.match_id = mp.match_id
                                AND r2.discord_id = mp.discord_id
                                AND r2.guild_id = m.guild_id)
              WHERE m.winning_team IS NOT NULL
                AND m.guild_id = ?1
                AND m.match_date >= datetime(?2, 'unixepoch')
                AND m.match_date < datetime(?3, 'unixepoch')
           ORDER BY m.match_date ASC, mp.match_id ASC, mp.discord_id ASC",
        )?;
        let mut matches = Vec::new();
        let mut players = BTreeMap::new();
        let mut rows = statement.query(params![guild_id, start_ts, end_ts])?;
        while let Some(row) = rows.next()? {
            let discord_id: i64 = row.get(1)?;
            let match_date: Option<String> = row.get(4)?;
            let item_counts: Option<String> = row.get(25)?;
            matches.push(DistinctMatch {
                match_id: row.get(0)?,
                discord_id,
                side: row.get(2)?,
                won: row.get::<_, Option<i64>>(3)?.map(|won| won != 0),
                finished_at: match_date.as_deref().and_then(parse_match_date),
                duration_seconds: row.get(5)?,
                hero_id: row.get(6)?,
                kills: row.get(7)?,
                deaths: row.get(8)?,
                assists: row.get(9)?,
                last_hits: row.get(10)?,
                gpm: row.get(11)?,
                hero_damage: row.get(12)?,
                tower_damage: row.get(13)?,
                hero_healing: row.get(14)?,
                wards: row.get(15)?,
                camps_stacked: row.get(16)?,
                rune_pickups: row.get(17)?,
                stuns: row.get(18)?,
                enriched: row.get(19)?,
                courier_kills: row.get(20)?,
                pings: row.get(21)?,
                actions_per_min: row.get(22)?,
                rapiers: row.get(23)?,
                buybacks: row.get(24)?,
                items: item_counts
                    .as_deref()
                    .map(parse_item_counts)
                    .unwrap_or_default(),
                comeback: row.get(26)?,
                throw: row.get(27)?,
                expected_win: row.get(28)?,
                rating_delta: row.get(29)?,
            });
            players
                .entry(discord_id)
                .or_insert(row.get::<_, Option<String>>(30)?);
        }
        let players = players
            .into_iter()
            .map(|(discord_id, timezone)| DistinctPlayer {
                discord_id,
                timezone,
            })
            .collect();
        Ok((matches, players))
    }

    /// Radiant gold advantage per minute, when the match was parsed.
    pub fn gold_advantage(
        &self,
        guild_id: i64,
        match_id: i64,
    ) -> Result<Option<Vec<f64>>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let raw: Option<String> = connection
            .query_row(
                "SELECT enrichment_data FROM matches WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, guild_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let series = raw
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|payload| {
                payload
                    .get("radiant_gold_adv")?
                    .as_array()?
                    .iter()
                    .map(serde_json::Value::as_f64)
                    .collect::<Option<Vec<_>>>()
            });
        Ok(series)
    }

    pub fn player_name(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, WrappedLiveRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT discord_username FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::copy_migrated_database;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn annual_betting_uses_settled_outcomes_and_charges_tax_on_the_first_win() {
        let file = NamedTempFile::new().unwrap();
        copy_migrated_database(file.path()).unwrap();
        let db = Connection::open(file.path()).unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO players(discord_id,guild_id,discord_username) VALUES(7,42,'Seven');
             INSERT INTO matches(match_id,team1_players,team2_players,winning_team,guild_id)
                VALUES(1,'[]','[]',1,42);
             INSERT INTO bet_settlement_taxes(match_id,guild_id,discord_id,vanity_tax,bankruptcy_penalty)
                VALUES(1,42,7,10,10);
             INSERT INTO bets(guild_id,match_id,discord_id,team_bet_on,amount,bet_time,payout)
                VALUES(42,1,7,'radiant',100,10,200),(42,1,7,'radiant',30,20,NULL),
                      (42,1,7,'dire',10,21,0);",
        ).unwrap();
        let repo = WrappedLiveRepository::new(file.path());
        let first = repo.betting_stats(42, 10, 20).unwrap().remove(0);
        assert_eq!((first.net_pnl, first.wins), (80, 1));
        let second = repo.betting_stats(42, 20, 22).unwrap().remove(0);
        assert_eq!((second.net_pnl, second.wins, second.losses), (20, 1, 1));
        let both = repo.betting_stats(42, 10, 22).unwrap().remove(0);
        assert_eq!((both.net_pnl, both.total_wagered), (100, 140));
        assert!(repo.betting_stats(99, 10, 22).unwrap().is_empty());
    }

    #[test]
    fn migrated_sqlite_projects_annual_summary_and_compact_enrichment() {
        let file = NamedTempFile::new().unwrap();
        copy_migrated_database(file.path()).unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(7,42,'Seven')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO matches(match_id,team1_players,team2_players,winning_team,match_date,guild_id,duration_seconds)
                 VALUES(1,'[]','[]',1,'2026-06-01 00:00:00',42,2400)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO match_participants(match_id,discord_id,guild_id,won,hero_id,kills,deaths,assists,gpm,xpm)
                 VALUES(1,7,42,1,2,10,2,15,600,700)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO wrapped_enrichment_facts(guild_id,match_id,discord_id,actions_per_min,lane_role)
                 VALUES(42,1,7,321,2)",
                [],
            )
            .unwrap();
        drop(connection);

        let repository = WrappedLiveRepository::new(file.path());
        let start = 1_767_225_600;
        let end = 1_798_761_600;
        assert_eq!(
            repository
                .server_summary(42, start, end)
                .unwrap()
                .total_matches,
            1
        );
        let rows = repository.player_year_matches(7, 42, start, end).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].enrichment_present);
        assert_eq!(rows[0].actions_per_min, Some(321.0));
        assert_eq!(rows[0].lane_role, Some(2));
    }

    #[test]
    fn player_aggregate_preserves_python_null_kda_semantics() {
        let file = NamedTempFile::new().unwrap();
        copy_migrated_database(file.path()).unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(7,42,'Seven')",
                [],
            )
            .unwrap();
        for match_id in 1..=2 {
            connection
                .execute(
                    "INSERT INTO matches(match_id,team1_players,team2_players,winning_team,match_date,guild_id)
                     VALUES(?1,'[]','[]',1,?2,42)",
                    params![match_id, format!("2026-0{match_id}-01 00:00:00")],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO match_participants(match_id,discord_id,guild_id,won,kills,deaths,assists)
                 VALUES(1,7,42,1,NULL,0,5),(2,7,42,0,5,0,5)",
                [],
            )
            .unwrap();
        drop(connection);
        let row = WrappedLiveRepository::new(file.path())
            .player_stats(42, 1_767_225_600, 1_798_761_600)
            .unwrap()
            .remove(0);
        assert_eq!(row.avg_kda, 10.0, "NULL row is excluded from SQLite AVG");
    }

    const START: i64 = 1_767_225_600;
    const END: i64 = 1_798_761_600;

    fn year_fixture() -> (NamedTempFile, Connection) {
        let file = NamedTempFile::new().unwrap();
        copy_migrated_database(file.path()).unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        (file, connection)
    }

    fn add_match(
        connection: &Connection,
        match_id: i64,
        guild_id: i64,
        date: &str,
        players: &[i64],
    ) {
        connection
            .execute(
                "INSERT INTO matches(match_id,team1_players,team2_players,winning_team,match_date,guild_id,duration_seconds)
                 VALUES(?1,'[]','[]',1,?2,?3,2400)",
                params![match_id, date, guild_id],
            )
            .unwrap();
        for player in players {
            connection
                .execute(
                    "INSERT OR IGNORE INTO players(discord_id,guild_id,discord_username,timezone)
                     VALUES(?1,?2,'P','Europe/Berlin')",
                    params![player, guild_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO match_participants(match_id,discord_id,guild_id,won,side,hero_id,kills,deaths,assists,gpm)
                     VALUES(?1,?2,?3,1,'radiant',5,3,2,1,500)",
                    params![match_id, player, guild_id],
                )
                .unwrap();
        }
    }

    #[test]
    fn guild_year_rows_covers_every_participant_and_isolates_guilds() {
        let (file, db) = year_fixture();
        add_match(&db, 1, 42, "2026-03-14T21:05:00+00:00", &[7, 8]);
        add_match(&db, 2, 99, "2026-03-14 21:05:00", &[7, 9]);
        let (matches, players) = WrappedLiveRepository::new(file.path())
            .guild_year_rows(42, START, END)
            .unwrap();
        let mut ids: Vec<_> = matches.iter().map(|m| (m.match_id, m.discord_id)).collect();
        ids.sort_unstable();
        assert_eq!(ids, [(1, 7), (1, 8)]);
        assert_eq!(players.len(), 2);
        assert_eq!(players[0].timezone.as_deref(), Some("Europe/Berlin"));
        let row = &matches[0];
        assert_eq!(row.finished_at, Some(1_773_522_300));
        assert_eq!(row.side.as_deref(), Some("radiant"));
        assert_eq!((row.won, row.duration_seconds), (Some(true), Some(2400)));
        assert_eq!(
            (row.kills, row.gpm, row.hero_id),
            (Some(3), Some(500), Some(5))
        );
    }

    #[test]
    fn guild_year_rows_respects_year_bounds() {
        let (file, db) = year_fixture();
        add_match(&db, 1, 42, "2025-12-31T23:59:59+00:00", &[7]);
        add_match(&db, 2, 42, "2025-12-31 23:59:59", &[7]);
        add_match(&db, 3, 42, "2026-01-01 00:00:00", &[7]);
        add_match(&db, 4, 42, "2026-12-31T23:59:59+00:00", &[7]);
        add_match(&db, 5, 42, "2027-01-01T00:00:00+00:00", &[7]);
        add_match(&db, 6, 42, "2027-01-01 00:00:00", &[7]);
        let (matches, _) = WrappedLiveRepository::new(file.path())
            .guild_year_rows(42, START, END)
            .unwrap();
        let mut ids: Vec<_> = matches.iter().map(|m| m.match_id).collect();
        ids.sort_unstable();
        assert_eq!(ids, [3, 4]);
        let finished: Vec<_> = matches.iter().map(|m| m.finished_at).collect();
        assert!(finished.contains(&Some(START)));
        assert!(finished.contains(&Some(END - 1)));
    }

    #[test]
    fn guild_year_rows_keeps_unenriched_values_null() {
        let (file, db) = year_fixture();
        add_match(&db, 1, 42, "2026-03-14 21:05:00", &[7, 8]);
        db.execute(
            "UPDATE match_participants SET obs_placed=3, sen_placed=NULL WHERE discord_id=8",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO wrapped_enrichment_facts(guild_id,match_id,discord_id,actions_per_min,
                 courier_kills,pings,rapier_count,buybacks,item_counts)
             VALUES(42,1,8,250,1,9,2,3,'{\"magic_wand\":2,\"blink\":1}')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO rating_history(discord_id,guild_id,rating_before,rating,timestamp,match_id,expected_team_win_prob)
             VALUES(8,42,1500,1520,'2026-03-14 21:10:00',1,0.4),
                   (8,42,1520,1530,'2026-03-14 21:11:00',1,0.45)",
            [],
        )
        .unwrap();
        let (matches, _) = WrappedLiveRepository::new(file.path())
            .guild_year_rows(42, START, END)
            .unwrap();
        let bare = matches.iter().find(|m| m.discord_id == 7).unwrap();
        assert!(!bare.enriched);
        assert_eq!(bare.wards, None);
        assert!(bare.items.is_empty());
        assert_eq!(
            (
                bare.rapiers,
                bare.buybacks,
                bare.expected_win,
                bare.rating_delta
            ),
            (None, None, None, None)
        );
        let full = matches.iter().find(|m| m.discord_id == 8).unwrap();
        assert!(full.enriched);
        assert_eq!(full.wards, Some(3));
        assert_eq!(
            full.items,
            [("blink".to_owned(), 1), ("magic_wand".to_owned(), 2)]
        );
        assert_eq!((full.rapiers, full.buybacks), (Some(2), Some(3)));
        assert_eq!((full.courier_kills, full.pings), (Some(1), Some(9)));
        assert_eq!(full.actions_per_min, Some(250.0));
        assert_eq!(full.expected_win, Some(0.45), "latest rating row wins");
        assert_eq!(full.rating_delta, Some(10.0));
        db.execute(
            "UPDATE wrapped_enrichment_facts SET item_counts='not json'",
            [],
        )
        .unwrap();
        let (matches, _) = WrappedLiveRepository::new(file.path())
            .guild_year_rows(42, START, END)
            .unwrap();
        let malformed = matches.iter().find(|m| m.discord_id == 8).unwrap();
        assert!(malformed.enriched && malformed.items.is_empty());
    }

    #[test]
    fn gold_advantage_reads_series_and_is_none_without_data() {
        let (file, db) = year_fixture();
        add_match(&db, 1, 42, "2026-03-14 21:05:00", &[7]);
        add_match(&db, 2, 42, "2026-03-15 21:05:00", &[7]);
        db.execute(
            "UPDATE matches SET enrichment_data='{\"radiant_gold_adv\":[0,150,-300.5]}' WHERE match_id=1",
            [],
        )
        .unwrap();
        let repo = WrappedLiveRepository::new(file.path());
        assert_eq!(
            repo.gold_advantage(42, 1).unwrap(),
            Some(vec![0.0, 150.0, -300.5])
        );
        assert_eq!(repo.gold_advantage(42, 2).unwrap(), None);
        assert_eq!(repo.gold_advantage(99, 1).unwrap(), None);
        assert_eq!(repo.gold_advantage(42, 404).unwrap(), None);
    }

    #[test]
    fn rating_history_is_year_scoped() {
        let (file, db) = year_fixture();
        for (n, ts) in [
            "2025-12-31 23:59:59",
            "2026-02-01 10:00:00",
            "2026-06-01 10:00:00",
            "2027-01-01 00:00:00",
        ]
        .into_iter()
        .enumerate()
        {
            db.execute(
                "INSERT INTO rating_history(discord_id,guild_id,rating_before,rating,timestamp,match_id)
                 VALUES(7,42,1500,?1,?2,?3)",
                params![1500 + n as i64, ts, n as i64 + 1],
            )
            .unwrap();
        }
        let rows = WrappedLiveRepository::new(file.path())
            .rating_history(7, 42, START, END)
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| matches!(row.rating, Some(1501.0 | 1502.0)))
        );
    }

    #[test]
    fn bankruptcies_count_this_years_events_only() {
        let (file, db) = year_fixture();
        db.execute_batch(&format!(
            "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(7,42,'Seven'),(8,42,'Eight');
             INSERT INTO bankruptcy_state(discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count)
                VALUES(7,42,{m},0,3),(8,42,{p},0,2);
             INSERT INTO economy_ledger_entries(guild_id,account_type,account_id,delta,balance_before,
                  balance_after,source,related_type,related_id,created_at)
             VALUES (42,'player',7,500,-500,0,'bankruptcy','bankruptcy','7',{p}),
                    (42,'player',7,500,-500,0,'bankruptcy','bankruptcy','7',{p2}),
                    (42,'player',7,500,-500,0,'bankruptcy','bankruptcy','7',{m}),
                    (42,'player',7,-20,100,80,'pet','pet','7',{m}),
                    (99,'player',7,500,-500,0,'bankruptcy','bankruptcy','7',{m}),
                    (42,'player',8,500,-500,0,'bankruptcy','bankruptcy','8',{p});",
            m = START + 86_400,
            p = START - 86_400,
            p2 = START - 2 * 86_400,
        ))
        .unwrap();
        let rows = WrappedLiveRepository::new(file.path())
            .bankruptcies(42, START, END)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].discord_id, rows[0].bankruptcy_count), (7, 1));
    }
}
