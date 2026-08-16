//! Read-only Wrapped queries over the Python-owned match schema.

use std::path::{Path, PathBuf};

use rusqlite::params;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedYearMatch {
    pub match_id: i64,
    pub discord_id: i64,
    pub hero_id: Option<i64>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub wrapped_facts_match_id: Option<i64>,
    pub actions_per_min: Option<i64>,
    pub lane_role: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WrappedMonthSummary {
    pub total_matches: i64,
    pub total_players: i64,
    pub unique_heroes: i64,
}

#[derive(Debug, Error)]
pub enum WrappedRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct WrappedRepository {
    path: PathBuf,
}

impl WrappedRepository {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn get_player_year_matches(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        year: i32,
        end_ts: i64,
    ) -> Result<Vec<WrappedYearMatch>, WrappedRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT mp.match_id,
                    mp.discord_id,
                    mp.hero_id,
                    mp.kills,
                    mp.deaths,
                    mp.assists,
                    wf.match_id,
                    wf.actions_per_min,
                    wf.lane_role
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
          LEFT JOIN wrapped_enrichment_facts AS wf
                 ON wf.guild_id = m.guild_id
                AND wf.match_id = mp.match_id
                AND wf.discord_id = mp.discord_id
              WHERE mp.discord_id = ?1
                AND m.guild_id = ?2
                AND m.winning_team IS NOT NULL
                AND m.match_date >= datetime(CAST(?3 AS TEXT) || '-01-01')
                AND m.match_date < datetime(?4, 'unixepoch')
           ORDER BY m.match_date ASC",
        )?;
        let rows = statement.query_map(
            params![discord_id, guild_id.unwrap_or(0), year, end_ts],
            |row| {
                Ok(WrappedYearMatch {
                    match_id: row.get(0)?,
                    discord_id: row.get(1)?,
                    hero_id: row.get(2)?,
                    kills: row.get(3)?,
                    deaths: row.get(4)?,
                    assists: row.get(5)?,
                    wrapped_facts_match_id: row.get(6)?,
                    actions_per_min: row.get(7)?,
                    lane_role: row.get(8)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(WrappedRepositoryError::from)
    }

    pub fn get_month_summary(
        &self,
        guild_id: Option<i64>,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<WrappedMonthSummary, WrappedRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT COUNT(DISTINCT m.match_id),
                        COUNT(DISTINCT mp.discord_id),
                        COUNT(DISTINCT mp.hero_id)
                   FROM matches AS m
              LEFT JOIN match_participants AS mp ON mp.match_id = m.match_id
                  WHERE m.winning_team IS NOT NULL
                    AND m.guild_id = ?1
                    AND m.match_date >= datetime(?2, 'unixepoch')
                    AND m.match_date < datetime(?3, 'unixepoch')",
                params![guild_id.unwrap_or(0), start_ts, end_ts],
                |row| {
                    Ok(WrappedMonthSummary {
                        total_matches: row.get(0)?,
                        total_players: row.get(1)?,
                        unique_heroes: row.get(2)?,
                    })
                },
            )
            .map_err(WrappedRepositoryError::from)
    }

    pub fn get_player_rating_change(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Option<f64>, WrappedRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT rating_before, rating
               FROM rating_history
              WHERE discord_id = ?1
                AND guild_id = ?2
                AND timestamp >= datetime(?3, 'unixepoch')
                AND timestamp < datetime(?4, 'unixepoch')
                AND (rating_before IS NOT NULL OR rating IS NOT NULL)
           ORDER BY timestamp ASC, id ASC",
        )?;
        let rows = statement
            .query_map(
                params![discord_id, guild_id.unwrap_or(0), start_ts, end_ts],
                |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let first = rows.first().and_then(|(before, after)| before.or(*after));
        let last = rows
            .iter()
            .rev()
            .find_map(|(before, after)| after.or(*before));
        Ok(first.zip(last).map(|(start, end)| end - start))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    fn fixture() -> NamedTempFile {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE matches (
                     match_id INTEGER PRIMARY KEY,
                     guild_id INTEGER NOT NULL,
                     winning_team INTEGER,
                     match_date TEXT NOT NULL
                 );
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     hero_id INTEGER,
                     kills INTEGER,
                     deaths INTEGER,
                     assists INTEGER
                 );
                 CREATE TABLE wrapped_enrichment_facts (
                     guild_id INTEGER NOT NULL,
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     actions_per_min INTEGER,
                     lane_role INTEGER
                 );
                 CREATE TABLE rating_history (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     rating_before REAL,
                     rating REAL,
                     timestamp TEXT NOT NULL
                 );",
            )
            .unwrap();
        for (match_id, kills) in [(1, 10), (2, 11), (3, 12)] {
            connection
                .execute(
                    "INSERT INTO matches(match_id,guild_id,winning_team,match_date)
                     VALUES (?1,42,1,?2)",
                    params![match_id, format!("2026-01-{match_id:02} 20:00:00")],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO match_participants(match_id,discord_id,hero_id,kills,deaths,assists)
                     VALUES (?1,100,?1,?2,3,5)",
                    params![match_id, kills],
                )
                .unwrap();
        }
        file
    }

    #[test]
    fn test_returns_matches_in_date_range() {
        let file = fixture();
        let repository = WrappedRepository::new(file.path());
        let end_ts = 1_798_761_600_i64;
        let rows = repository
            .get_player_year_matches(100, Some(42), 2026, end_ts)
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.discord_id == 100));
        assert_eq!(
            rows.iter()
                .filter_map(|row| row.kills)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([10, 11, 12])
        );
        assert!(
            repository
                .get_player_year_matches(100, Some(99_999), 2026, end_ts)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn none_guild_normalizes_to_zero_and_reads_compact_enrichment() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute("UPDATE matches SET guild_id=0 WHERE match_id=1", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO wrapped_enrichment_facts
                 (guild_id,match_id,discord_id,actions_per_min,lane_role)
                 VALUES (0,1,100,200,2)",
                [],
            )
            .unwrap();
        drop(connection);
        let row = WrappedRepository::new(file.path())
            .get_player_year_matches(100, None, 2026, 1_798_761_600)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(row.actions_per_min, Some(200));
        assert_eq!(row.lane_role, Some(2));
    }

    #[test]
    fn test_get_month_summary_empty() {
        let file = fixture();
        let summary = WrappedRepository::new(file.path())
            .get_month_summary(Some(0), 1_700_000_000, 1_702_592_000)
            .unwrap();
        assert_eq!(summary.total_matches, 0);
    }

    #[test]
    fn test_get_player_rating_change_uses_first_and_last_in_period() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        let rows = [
            (111, 42, Some(1300.0), Some(1320.0), "2025-12-31 12:00:00"),
            (111, 42, None, Some(1400.0), "2026-01-01 12:00:00"),
            (111, 42, Some(1400.0), Some(1420.0), "2026-01-02 12:00:00"),
            (111, 42, Some(1420.0), None, "2026-06-01 12:00:00"),
            (111, 42, Some(1480.0), Some(1505.0), "2026-12-30 12:00:00"),
            (111, 42, Some(1505.0), Some(1510.0), "2027-01-01 12:00:00"),
            (111, 7, Some(900.0), Some(2000.0), "2026-07-01 12:00:00"),
            (222, 42, Some(1000.0), Some(2000.0), "2026-07-01 12:00:00"),
        ];
        for row in rows {
            connection
                .execute(
                    "INSERT INTO rating_history
                     (discord_id,guild_id,rating_before,rating,timestamp)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![row.0, row.1, row.2, row.3, row.4],
                )
                .unwrap();
        }
        drop(connection);
        let repository = WrappedRepository::new(file.path());
        assert_eq!(
            repository
                .get_player_rating_change(111, Some(42), 1_767_225_600, 1_798_761_600)
                .unwrap(),
            Some(105.0)
        );
        assert_eq!(
            repository
                .get_player_rating_change(999, Some(42), 1_767_225_600, 1_798_761_600)
                .unwrap(),
            None
        );
    }
}
