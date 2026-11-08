//! Read-only Wrapped queries over the match schema.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use thiserror::Error;

use crate::open_runtime_connection;

/// A player's average positive APM across completed, enriched matches.
///
/// The sample count is the number of enriched match-participant facts that
/// contributed to the average.  Rows with missing, zero, or negative APM are
/// deliberately excluded by the repository query.
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedApmSummary {
    pub discord_id: i64,
    pub average_apm: f64,
    pub games: i64,
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

    /// Return one player's APM summary for a normalized guild.
    ///
    /// Facts are joined to both the completed match and its participant row so
    /// stale facts from deleted/unrecorded matches cannot contribute.  The
    /// persisted fact itself is the enrichment marker; raw match JSON is not
    /// required.  A `None` guild follows the repository-wide DM/global
    /// sentinel convention and is normalized to guild `0`.
    pub fn get_player_apm_summary(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<WrappedApmSummary>, WrappedRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT wf.discord_id,
                        AVG(CAST(wf.actions_per_min AS REAL)),
                        COUNT(*)
                   FROM wrapped_enrichment_facts AS wf
                   JOIN matches AS m
                     ON m.match_id = wf.match_id
                    AND m.guild_id = wf.guild_id
                   JOIN match_participants AS mp
                     ON mp.match_id = wf.match_id
                    AND mp.guild_id = wf.guild_id
                    AND mp.discord_id = wf.discord_id
                  WHERE wf.guild_id = ?1
                    AND wf.discord_id = ?2
                    AND m.winning_team IS NOT NULL
                    AND wf.actions_per_min IS NOT NULL
                    AND wf.actions_per_min > 0
               GROUP BY wf.discord_id",
                params![guild_id.unwrap_or(0), discord_id],
                |row| {
                    Ok(WrappedApmSummary {
                        discord_id: row.get(0)?,
                        average_apm: row.get(1)?,
                        games: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(WrappedRepositoryError::from)
    }

    /// Return the APM leaderboard for a normalized guild.
    ///
    /// Entries are ordered by average APM, then Discord ID for deterministic
    /// ties.  A non-positive limit returns an empty list.
    pub fn get_apm_leaderboard(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<WrappedApmSummary>, WrappedRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT wf.discord_id,
                    AVG(CAST(wf.actions_per_min AS REAL)),
                    COUNT(*)
               FROM wrapped_enrichment_facts AS wf
               JOIN matches AS m
                 ON m.match_id = wf.match_id
                AND m.guild_id = wf.guild_id
               JOIN match_participants AS mp
                 ON mp.match_id = wf.match_id
                AND mp.guild_id = wf.guild_id
                AND mp.discord_id = wf.discord_id
              WHERE wf.guild_id = ?1
                AND m.winning_team IS NOT NULL
                AND wf.actions_per_min IS NOT NULL
                AND wf.actions_per_min > 0
           GROUP BY wf.discord_id
           ORDER BY AVG(CAST(wf.actions_per_min AS REAL)) DESC,
                    wf.discord_id ASC
              LIMIT ?2",
        )?;
        statement
            .query_map(params![guild_id.unwrap_or(0), limit.max(0)], |row| {
                Ok(WrappedApmSummary {
                    discord_id: row.get(0)?,
                    average_apm: row.get(1)?,
                    games: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(WrappedRepositoryError::from)
    }
}

#[cfg(test)]
mod tests {
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
                     match_date TEXT NOT NULL,
                     enrichment_data TEXT
                 );
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
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
                    "INSERT INTO match_participants(match_id,discord_id,guild_id,hero_id,kills,deaths,assists)
                     VALUES (?1,100,42,?1,?2,3,5)",
                    params![match_id, kills],
                )
                .unwrap();
        }
        file
    }

    #[test]
    fn apm_summary_averages_only_positive_facts_from_completed_enriched_matches() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "UPDATE matches SET enrichment_data='{}' WHERE match_id IN (1,2,3);
                 INSERT INTO wrapped_enrichment_facts
                     (guild_id,match_id,discord_id,actions_per_min)
                 VALUES
                     (42,1,100,100),
                     (42,2,100,200),
                     (42,3,100,0),
                     (42,99,100,900);",
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            WrappedRepository::new(file.path())
                .get_player_apm_summary(100, Some(42))
                .unwrap(),
            Some(WrappedApmSummary {
                discord_id: 100,
                average_apm: 150.0,
                games: 2,
            })
        );
    }

    #[test]
    fn apm_summary_excludes_unrecorded_and_non_participant_facts() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "UPDATE matches SET enrichment_data='{}' WHERE match_id IN (1,2,3);
                 UPDATE matches SET winning_team=NULL WHERE match_id=2;
                 UPDATE matches SET enrichment_data=NULL WHERE match_id=3;
                 INSERT INTO wrapped_enrichment_facts
                     (guild_id,match_id,discord_id,actions_per_min)
                 VALUES
                     (42,1,999,500),
                     (42,2,100,200);",
            )
            .unwrap();
        drop(connection);

        let repository = WrappedRepository::new(file.path());
        assert_eq!(
            repository.get_player_apm_summary(100, Some(42)).unwrap(),
            None
        );
        assert_eq!(
            repository.get_player_apm_summary(999, Some(42)).unwrap(),
            None
        );
    }

    #[test]
    fn apm_leaderboard_is_guild_scoped_and_deterministically_sorted() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "UPDATE matches SET enrichment_data='{}' WHERE match_id IN (1,2,3);
                 INSERT INTO matches(match_id,guild_id,winning_team,match_date,enrichment_data)
                 VALUES (4,7,1,'2026-01-04 20:00:00','{}'),
                        (5,42,1,'2026-01-05 20:00:00','{}');
                 INSERT INTO match_participants(match_id,discord_id,guild_id)
                 VALUES (4,200,7),(5,200,42),(5,300,42);
                 INSERT INTO wrapped_enrichment_facts
                     (guild_id,match_id,discord_id,actions_per_min)
                 VALUES
                     (42,1,100,200),
                     (42,2,100,100),
                     (42,3,200,150),
                     (42,5,200,200),
                     (42,5,300,200),
                     (7,4,200,999);",
            )
            .unwrap();
        drop(connection);

        let repository = WrappedRepository::new(file.path());
        assert_eq!(
            repository.get_apm_leaderboard(Some(42), 10).unwrap(),
            [
                WrappedApmSummary {
                    discord_id: 200,
                    average_apm: 200.0,
                    games: 1,
                },
                WrappedApmSummary {
                    discord_id: 300,
                    average_apm: 200.0,
                    games: 1,
                },
                WrappedApmSummary {
                    discord_id: 100,
                    average_apm: 150.0,
                    games: 2,
                },
            ]
        );
        assert_eq!(
            repository.get_apm_leaderboard(None, 10).unwrap(),
            Vec::<WrappedApmSummary>::new()
        );
        assert_eq!(
            repository.get_apm_leaderboard(Some(42), 0).unwrap(),
            Vec::<WrappedApmSummary>::new()
        );
        assert_eq!(
            repository.get_player_apm_summary(200, Some(7)).unwrap(),
            Some(WrappedApmSummary {
                discord_id: 200,
                average_apm: 999.0,
                games: 1,
            })
        );
    }
}
