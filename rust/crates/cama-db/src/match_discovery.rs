//! Existing-schema persistence used by match discovery administration.
//!
//! Python owns every schema migration. Production paths in this module only
//! open an already-existing database through [`crate::open_runtime_connection`].
//! DDL appears solely in disposable test fixtures.

use std::path::{Path, PathBuf};

#[cfg(test)]
use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, PartialEq)]
pub struct MatchEnrichmentMetadata {
    pub match_id: i64,
    pub guild_id: i64,
    pub valve_match_id: Option<i64>,
    pub enrichment_source: Option<String>,
    pub enrichment_confidence: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchEnrichmentUpdate {
    pub match_id: i64,
    pub guild_id: Option<i64>,
    pub valve_match_id: i64,
    pub duration_seconds: i64,
    pub radiant_score: i64,
    pub dire_score: i64,
    pub game_mode: i64,
    pub enrichment_source: String,
    pub enrichment_confidence: Option<f64>,
}

#[derive(Debug, Error)]
pub enum MatchDiscoveryRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct MatchDiscoveryRepository {
    path: PathBuf,
    #[cfg(test)]
    trace: Option<fn(TraceEvent<'_>)>,
}

impl MatchDiscoveryRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            #[cfg(test)]
            trace: None,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        let connection = open_runtime_connection(&self.path)?;
        #[cfg(test)]
        connection.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, self.trace);
        Ok(connection)
    }

    #[cfg(test)]
    fn with_trace(path: impl AsRef<Path>, trace: fn(TraceEvent<'_>)) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            trace: Some(trace),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    pub fn update_match_enrichment(
        &self,
        update: &MatchEnrichmentUpdate,
    ) -> Result<bool, MatchDiscoveryRepositoryError> {
        let connection = self.connection()?;
        let changed = if let Some(guild_id) = update.guild_id {
            connection.execute(
                "UPDATE matches
                    SET valve_match_id = ?1,
                        duration_seconds = ?2,
                        radiant_score = ?3,
                        dire_score = ?4,
                        game_mode = ?5,
                        enrichment_source = ?6,
                        enrichment_confidence = ?7
                  WHERE match_id = ?8 AND guild_id = ?9",
                params![
                    update.valve_match_id,
                    update.duration_seconds,
                    update.radiant_score,
                    update.dire_score,
                    update.game_mode,
                    update.enrichment_source,
                    update.enrichment_confidence,
                    update.match_id,
                    Self::normalize_guild_id(Some(guild_id)),
                ],
            )?
        } else {
            connection.execute(
                "UPDATE matches
                    SET valve_match_id = ?1,
                        duration_seconds = ?2,
                        radiant_score = ?3,
                        dire_score = ?4,
                        game_mode = ?5,
                        enrichment_source = ?6,
                        enrichment_confidence = ?7
                  WHERE match_id = ?8",
                params![
                    update.valve_match_id,
                    update.duration_seconds,
                    update.radiant_score,
                    update.dire_score,
                    update.game_mode,
                    update.enrichment_source,
                    update.enrichment_confidence,
                    update.match_id,
                ],
            )?
        };
        Ok(changed != 0)
    }

    pub fn enrichment_metadata(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<MatchEnrichmentMetadata>, MatchDiscoveryRepositoryError> {
        let connection = self.connection()?;
        let row = if let Some(guild_id) = guild_id {
            connection
                .query_row(
                    "SELECT match_id, guild_id, valve_match_id,
                            enrichment_source, enrichment_confidence
                       FROM matches
                      WHERE match_id = ?1 AND guild_id = ?2",
                    params![match_id, Self::normalize_guild_id(Some(guild_id))],
                    metadata_from_row,
                )
                .optional()?
        } else {
            connection
                .query_row(
                    "SELECT match_id, guild_id, valve_match_id,
                            enrichment_source, enrichment_confidence
                       FROM matches
                      WHERE match_id = ?1",
                    [match_id],
                    metadata_from_row,
                )
                .optional()?
        };
        Ok(row)
    }

    /// Clear match-level and dependent enrichment in one transaction.
    pub fn wipe_match_enrichment(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchDiscoveryRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = if let Some(guild_id) = guild_id {
            transaction.execute(
                &format!(
                    "{} WHERE match_id = ?1 AND guild_id = ?2",
                    clear_match_enrichment_sql()
                ),
                params![match_id, Self::normalize_guild_id(Some(guild_id))],
            )?
        } else {
            transaction.execute(
                &format!("{} WHERE match_id = ?1", clear_match_enrichment_sql()),
                [match_id],
            )?
        };
        if changed == 0 {
            transaction.rollback()?;
            return Ok(false);
        }

        clear_dependent_enrichment(&transaction, &[match_id])?;
        if let Some(guild_id) = guild_id {
            transaction.execute(
                "DELETE FROM wrapped_enrichment_facts
                  WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, Self::normalize_guild_id(Some(guild_id))],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM wrapped_enrichment_facts WHERE match_id = ?1",
                [match_id],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Wipe only auto-discovered matches in one normalized guild.
    pub fn wipe_auto_discovered_enrichments(
        &self,
        guild_id: Option<i64>,
    ) -> Result<usize, MatchDiscoveryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let match_ids = {
            let mut statement = transaction.prepare(
                "SELECT match_id
                   FROM matches
                  WHERE guild_id = ?1 AND enrichment_source = 'auto'
                  ORDER BY match_id",
            )?;
            statement
                .query_map([guild_id], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if match_ids.is_empty() {
            transaction.commit()?;
            return Ok(0);
        }

        transaction.execute(
            &format!(
                "{} WHERE guild_id = ?1 AND enrichment_source = 'auto'",
                clear_match_enrichment_sql()
            ),
            [guild_id],
        )?;
        clear_dependent_enrichment(&transaction, &match_ids)?;

        let placeholders = repeat_placeholders(match_ids.len());
        let sql = format!(
            "DELETE FROM wrapped_enrichment_facts
              WHERE guild_id = ? AND match_id IN ({placeholders})"
        );
        let parameters = std::iter::once(guild_id).chain(match_ids.iter().copied());
        transaction.execute(&sql, params_from_iter(parameters))?;
        transaction.commit()?;
        Ok(match_ids.len())
    }

    pub fn get_auto_discovered_count(
        &self,
        guild_id: Option<i64>,
    ) -> Result<i64, MatchDiscoveryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COUNT(*)
                   FROM matches
                  WHERE guild_id = ?1 AND enrichment_source = 'auto'",
                [guild_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Count every enriched match in one normalized guild, regardless of
    /// whether the source was manual or automatic.
    pub fn get_enriched_count(
        &self,
        guild_id: Option<i64>,
    ) -> Result<i64, MatchDiscoveryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COUNT(*) FROM matches
                  WHERE guild_id = ?1 AND valve_match_id IS NOT NULL",
                [guild_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Clear all match, participant, ban, and Wrapped enrichment projections
    /// for one guild under a single immediate writer lock.
    pub fn wipe_all_enrichments(
        &self,
        guild_id: Option<i64>,
    ) -> Result<usize, MatchDiscoveryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let match_ids = {
            let mut statement = transaction.prepare(
                "SELECT match_id FROM matches
                  WHERE guild_id = ?1 AND valve_match_id IS NOT NULL
                  ORDER BY match_id",
            )?;
            statement
                .query_map([guild_id], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if match_ids.is_empty() {
            transaction.commit()?;
            return Ok(0);
        }

        transaction.execute(
            &format!(
                "{} WHERE guild_id = ?1 AND valve_match_id IS NOT NULL",
                clear_match_enrichment_sql()
            ),
            [guild_id],
        )?;
        clear_dependent_enrichment(&transaction, &match_ids)?;
        let placeholders = repeat_placeholders(match_ids.len());
        let sql = format!(
            "DELETE FROM wrapped_enrichment_facts
              WHERE guild_id = ? AND match_id IN ({placeholders})"
        );
        let parameters = std::iter::once(guild_id).chain(match_ids.iter().copied());
        transaction.execute(&sql, params_from_iter(parameters))?;
        transaction.commit()?;
        Ok(match_ids.len())
    }

    /// Return `(participant_count, participants_with_fantasy)` for the exact
    /// guild-scoped match. The OpenSkill phase-two adapter uses this to retain
    /// Python's all-ten-or-skip guard before requesting a durable replay.
    pub fn fantasy_completion(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(usize, usize), MatchDiscoveryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let (total, complete) = connection.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN mp.fantasy_points IS NOT NULL THEN 1 ELSE 0 END)
               FROM match_participants AS mp
               JOIN matches AS m ON m.match_id = mp.match_id
              WHERE mp.match_id = ?1 AND m.guild_id = ?2",
            params![match_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                ))
            },
        )?;
        Ok((
            usize::try_from(total).unwrap_or_default(),
            usize::try_from(complete).unwrap_or_default(),
        ))
    }
}

fn metadata_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MatchEnrichmentMetadata> {
    Ok(MatchEnrichmentMetadata {
        match_id: row.get(0)?,
        guild_id: row.get(1)?,
        valve_match_id: row.get(2)?,
        enrichment_source: row.get(3)?,
        enrichment_confidence: row.get(4)?,
    })
}

fn clear_match_enrichment_sql() -> &'static str {
    "UPDATE matches
        SET valve_match_id = NULL,
            duration_seconds = NULL,
            radiant_score = NULL,
            dire_score = NULL,
            game_mode = NULL,
            enrichment_data = NULL,
            enrichment_source = NULL,
            enrichment_confidence = NULL"
}

fn clear_dependent_enrichment(
    transaction: &rusqlite::Transaction<'_>,
    match_ids: &[i64],
) -> Result<(), rusqlite::Error> {
    if match_ids.is_empty() {
        return Ok(());
    }
    let placeholders = repeat_placeholders(match_ids.len());
    let participant_sql = format!(
        "UPDATE match_participants
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
          WHERE match_id IN ({placeholders})"
    );
    transaction.execute(
        &participant_sql,
        params_from_iter(match_ids.iter().copied()),
    )?;
    let bans_sql = format!("DELETE FROM match_bans WHERE match_id IN ({placeholders})");
    transaction.execute(&bans_sql, params_from_iter(match_ids.iter().copied()))?;
    Ok(())
}

fn repeat_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    const GUILD: i64 = 86_753_090;
    static TRACE_TEST_LOCK: Mutex<()> = Mutex::new(());
    static TRACE_STATEMENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn capture_trace(event: TraceEvent<'_>) {
        if let TraceEvent::Stmt(statement, fallback) = event {
            TRACE_STATEMENTS
                .lock()
                .expect("trace statements lock")
                .push(
                    statement
                        .expanded_sql()
                        .unwrap_or_else(|| fallback.to_owned()),
                );
        }
    }

    fn assert_immediate_lock_precedes_match_selection(statements: &[String]) {
        let statements = statements
            .iter()
            .map(|statement| {
                statement
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_uppercase()
            })
            .collect::<Vec<_>>();
        let begin = statements
            .iter()
            .position(|statement| statement == "BEGIN IMMEDIATE")
            .expect("immediate transaction trace");
        let selection = statements
            .iter()
            .position(|statement| statement.starts_with("SELECT MATCH_ID FROM MATCHES"))
            .expect("match selection trace");
        assert_eq!(
            statements
                .iter()
                .filter(|statement| *statement == "BEGIN IMMEDIATE")
                .count(),
            1
        );
        assert!(begin < selection, "trace order was {statements:?}");
    }

    fn repository() -> (NamedTempFile, MatchDiscoveryRepository) {
        let file = NamedTempFile::new().expect("create disposable discovery database");
        let connection = Connection::open(file.path()).expect("open disposable database");
        connection
            .execute_batch(
                "CREATE TABLE matches (
                     match_id INTEGER PRIMARY KEY,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     valve_match_id INTEGER,
                     duration_seconds INTEGER,
                     radiant_score INTEGER,
                     dire_score INTEGER,
                     game_mode INTEGER,
                     enrichment_data TEXT,
                     enrichment_source TEXT,
                     enrichment_confidence REAL
                 );
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     hero_id INTEGER,
                     kills INTEGER,
                     deaths INTEGER,
                     assists INTEGER,
                     gpm INTEGER,
                     xpm INTEGER,
                     hero_damage INTEGER,
                     tower_damage INTEGER,
                     last_hits INTEGER,
                     denies INTEGER,
                     net_worth INTEGER,
                     hero_healing INTEGER,
                     lane_role INTEGER,
                     lane_efficiency REAL,
                     towers_killed INTEGER,
                     roshans_killed INTEGER,
                     teamfight_participation REAL,
                     obs_placed INTEGER,
                     sen_placed INTEGER,
                     camps_stacked INTEGER,
                     rune_pickups INTEGER,
                     firstblood_claimed INTEGER,
                     stuns REAL,
                     fantasy_points REAL
                 );
                 CREATE TABLE match_bans (match_id INTEGER NOT NULL, discord_id INTEGER);
                 CREATE TABLE wrapped_enrichment_facts (
                     match_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     discord_id INTEGER
                 );",
            )
            .expect("create Python-owned shapes in disposable fixture");
        drop(connection);
        let repository = MatchDiscoveryRepository::new(file.path());
        (file, repository)
    }

    fn insert_match(file: &NamedTempFile, match_id: i64, guild_id: i64) {
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute(
                "INSERT INTO matches (match_id, guild_id) VALUES (?1, ?2)",
                params![match_id, guild_id],
            )
            .expect("insert internal match");
        connection
            .execute(
                "INSERT INTO match_participants (
                     match_id, discord_id, hero_id, kills, fantasy_points
                 ) VALUES (?1, ?2, 1, 5, 7.5)",
                params![match_id, match_id * 10],
            )
            .expect("insert enriched participant");
    }

    fn enrich(
        repository: &MatchDiscoveryRepository,
        match_id: i64,
        guild_id: i64,
        source: &str,
        confidence: Option<f64>,
    ) {
        assert!(
            repository
                .update_match_enrichment(&MatchEnrichmentUpdate {
                    match_id,
                    guild_id: Some(guild_id),
                    valve_match_id: match_id * 100,
                    duration_seconds: 2_400,
                    radiant_score: 35,
                    dire_score: 22,
                    game_mode: 2,
                    enrichment_source: source.to_owned(),
                    enrichment_confidence: confidence,
                })
                .expect("enrich fixture match")
        );
    }

    #[test]
    fn test_wipe_match_enrichment() {
        let (file, repository) = repository();
        insert_match(&file, 1, GUILD);
        enrich(&repository, 1, GUILD, "manual", None);
        assert_eq!(
            repository
                .enrichment_metadata(1, Some(GUILD))
                .expect("load metadata")
                .expect("match")
                .valve_match_id,
            Some(100)
        );

        assert!(
            repository
                .wipe_match_enrichment(1, None)
                .expect("wipe enrichment")
        );

        assert_eq!(
            repository
                .enrichment_metadata(1, Some(GUILD))
                .expect("load wiped metadata")
                .expect("match")
                .valve_match_id,
            None
        );
    }

    #[test]
    fn test_wipe_is_scoped_to_guild() {
        let (file, repository) = repository();
        insert_match(&file, 501, 100);
        insert_match(&file, 502, 200);
        enrich(&repository, 501, 100, "auto", None);
        enrich(&repository, 502, 200, "auto", None);

        assert!(
            !repository
                .wipe_match_enrichment(502, Some(100))
                .expect("wrong guild is a no-op")
        );
        assert_eq!(
            repository
                .enrichment_metadata(502, Some(200))
                .expect("load other guild")
                .expect("other match")
                .valve_match_id,
            Some(50_200)
        );

        assert!(
            repository
                .wipe_match_enrichment(502, Some(200))
                .expect("owning guild wipes")
        );
        let metadata = repository
            .enrichment_metadata(502, Some(200))
            .expect("load wiped match")
            .expect("match remains");
        assert_eq!(metadata.valve_match_id, None);
        assert_eq!(metadata.enrichment_source, None);
    }

    #[test]
    fn test_wipe_match_enrichment_not_found() {
        let (_file, repository) = repository();

        assert!(
            !repository
                .wipe_match_enrichment(99_999, None)
                .expect("missing match is not an error")
        );
    }

    #[test]
    fn test_wipe_auto_discovered_enrichments() {
        let (file, repository) = repository();
        insert_match(&file, 1, GUILD);
        insert_match(&file, 2, GUILD);
        insert_match(&file, 3, GUILD);
        enrich(&repository, 1, GUILD, "manual", None);
        enrich(&repository, 2, GUILD, "auto", Some(0.9));
        enrich(&repository, 3, GUILD, "auto", Some(0.85));

        assert_eq!(
            repository
                .wipe_auto_discovered_enrichments(Some(GUILD))
                .expect("wipe auto discovery"),
            2
        );

        assert_eq!(
            repository
                .enrichment_metadata(1, Some(GUILD))
                .expect("manual metadata")
                .expect("manual match")
                .valve_match_id,
            Some(100)
        );
        for match_id in [2, 3] {
            assert_eq!(
                repository
                    .enrichment_metadata(match_id, Some(GUILD))
                    .expect("auto metadata")
                    .expect("auto match")
                    .valve_match_id,
                None
            );
        }
    }

    #[test]
    fn test_get_auto_discovered_count() {
        let (file, repository) = repository();
        assert_eq!(
            repository
                .get_auto_discovered_count(Some(GUILD))
                .expect("initial count"),
            0
        );
        insert_match(&file, 1, GUILD);
        enrich(&repository, 1, GUILD, "auto", Some(0.9));

        assert_eq!(
            repository
                .get_auto_discovered_count(Some(GUILD))
                .expect("auto count"),
            1
        );
    }

    #[test]
    fn all_enrichment_count_and_wipe_include_manual_and_auto_rows() {
        let (file, repository) = repository();
        for match_id in 1..=3 {
            insert_match(&file, match_id, GUILD);
        }
        enrich(&repository, 1, GUILD, "manual", None);
        enrich(&repository, 2, GUILD, "auto", Some(0.9));

        assert_eq!(repository.get_enriched_count(Some(GUILD)).unwrap(), 2);
        assert_eq!(repository.wipe_all_enrichments(Some(GUILD)).unwrap(), 2);
        assert_eq!(repository.get_enriched_count(Some(GUILD)).unwrap(), 0);
        for match_id in [1, 2] {
            assert_eq!(
                repository
                    .enrichment_metadata(match_id, Some(GUILD))
                    .unwrap()
                    .unwrap()
                    .valve_match_id,
                None
            );
        }
    }

    #[test]
    fn fantasy_completion_is_guild_and_match_scoped() {
        let (file, repository) = repository();
        insert_match(&file, 1, GUILD);
        insert_match(&file, 2, GUILD + 1);
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute(
                "UPDATE match_participants SET fantasy_points = NULL
                  WHERE match_id = 2",
                [],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            repository.fantasy_completion(1, Some(GUILD)).unwrap(),
            (1, 1)
        );
        assert_eq!(
            repository.fantasy_completion(2, Some(GUILD + 1)).unwrap(),
            (1, 0)
        );
        assert_eq!(
            repository.fantasy_completion(2, Some(GUILD)).unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn test_enrichment_source_and_confidence_stored() {
        let (file, repository) = repository();
        insert_match(&file, 1, GUILD);

        enrich(&repository, 1, GUILD, "auto", Some(0.85));

        let metadata = repository
            .enrichment_metadata(1, Some(GUILD))
            .expect("load metadata")
            .expect("match");
        assert_eq!(metadata.enrichment_source.as_deref(), Some("auto"));
        assert_eq!(metadata.enrichment_confidence, Some(0.85));
    }

    #[test]
    fn stronger_wipe_is_guild_scoped_for_signed_ids() {
        let (file, repository) = repository();
        insert_match(&file, -1, -42);
        insert_match(&file, -2, 42);
        enrich(&repository, -1, -42, "auto", Some(1.0));
        enrich(&repository, -2, 42, "auto", Some(1.0));

        assert_eq!(
            repository
                .wipe_auto_discovered_enrichments(Some(-42))
                .expect("wipe signed guild"),
            1
        );
        assert_eq!(
            repository
                .get_auto_discovered_count(Some(-42))
                .expect("signed count"),
            0
        );
        assert_eq!(
            repository
                .get_auto_discovered_count(Some(42))
                .expect("other count"),
            1
        );
    }

    #[test]
    fn test_enrichment_wipes_delete_normalized_bans() {
        let (file, repository) = repository();
        insert_match(&file, 11, GUILD);
        insert_match(&file, 12, GUILD);
        enrich(&repository, 11, GUILD, "auto", Some(1.0));
        enrich(&repository, 12, GUILD, "manual", None);
        let connection = Connection::open(file.path()).expect("open fixture");
        for (match_id, discord_id) in [(11, 101), (12, 202)] {
            connection
                .execute(
                    "INSERT INTO match_bans (match_id, discord_id) VALUES (?1, ?2)",
                    params![match_id, discord_id],
                )
                .expect("insert normalized ban");
        }
        drop(connection);

        assert_eq!(
            repository
                .wipe_auto_discovered_enrichments(Some(GUILD))
                .expect("wipe automatic enrichment"),
            1
        );
        let connection = Connection::open(file.path()).expect("inspect automatic wipe");
        let rows = connection
            .prepare("SELECT match_id FROM match_bans ORDER BY match_id")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .expect("read remaining bans");
        assert_eq!(rows, [12]);
        drop(connection);

        assert_eq!(
            repository
                .wipe_all_enrichments(Some(GUILD))
                .expect("wipe every enrichment"),
            1
        );
        let remaining: i64 = Connection::open(file.path())
            .expect("inspect full wipe")
            .query_row("SELECT COUNT(*) FROM match_bans", [], |row| row.get(0))
            .expect("count remaining bans");
        assert_eq!(remaining, 0);
    }

    #[test]
    fn test_single_enrichment_wipe_deletes_normalized_bans() {
        let (file, repository) = repository();
        insert_match(&file, 21, GUILD);
        enrich(&repository, 21, GUILD, "manual", None);
        Connection::open(file.path())
            .expect("open fixture")
            .execute(
                "INSERT INTO match_bans (match_id, discord_id) VALUES (21, 101)",
                [],
            )
            .expect("insert normalized ban");

        assert!(
            repository
                .wipe_match_enrichment(21, Some(GUILD))
                .expect("wipe one enrichment")
        );
        let remaining: i64 = Connection::open(file.path())
            .expect("inspect single wipe")
            .query_row("SELECT COUNT(*) FROM match_bans", [], |row| row.get(0))
            .expect("count remaining bans");
        assert_eq!(remaining, 0);
    }

    #[test]
    fn test_bulk_wipes_remove_only_their_compact_fact_rows() {
        let (file, repository) = repository();
        insert_match(&file, 31, GUILD);
        insert_match(&file, 32, GUILD);
        enrich(&repository, 31, GUILD, "auto", Some(1.0));
        enrich(&repository, 32, GUILD, "manual", None);
        let connection = Connection::open(file.path()).expect("open fixture");
        for (match_id, discord_id) in [(31, 101), (32, 202)] {
            connection
                .execute(
                    "INSERT INTO wrapped_enrichment_facts
                         (match_id, guild_id, discord_id)
                     VALUES (?1, ?2, ?3)",
                    params![match_id, GUILD, discord_id],
                )
                .expect("insert compact fact");
        }
        drop(connection);

        assert_eq!(
            repository
                .wipe_auto_discovered_enrichments(Some(GUILD))
                .expect("wipe automatic facts"),
            1
        );
        let connection = Connection::open(file.path()).expect("inspect automatic wipe");
        let rows = connection
            .prepare("SELECT match_id FROM wrapped_enrichment_facts ORDER BY match_id")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .expect("read remaining facts");
        assert_eq!(rows, [32]);
        drop(connection);

        assert_eq!(
            repository
                .wipe_all_enrichments(Some(GUILD))
                .expect("wipe all facts"),
            1
        );
        let remaining: i64 = Connection::open(file.path())
            .expect("inspect full wipe")
            .query_row("SELECT COUNT(*) FROM wrapped_enrichment_facts", [], |row| {
                row.get(0)
            })
            .expect("count compact facts");
        assert_eq!(remaining, 0);
    }

    #[test]
    fn test_bulk_wipes_lock_before_selecting_match_ids_auto() {
        let _serial = TRACE_TEST_LOCK.lock().expect("serialize trace test");
        let (file, _) = repository();
        let repository = MatchDiscoveryRepository::with_trace(file.path(), capture_trace);
        insert_match(&file, 41, GUILD);
        enrich(&repository, 41, GUILD, "auto", Some(1.0));
        TRACE_STATEMENTS
            .lock()
            .expect("trace statements lock")
            .clear();

        assert_eq!(
            repository
                .wipe_auto_discovered_enrichments(Some(GUILD))
                .expect("wipe automatic enrichment"),
            1
        );
        assert_immediate_lock_precedes_match_selection(
            &TRACE_STATEMENTS.lock().expect("trace statements lock"),
        );
    }

    #[test]
    fn test_bulk_wipes_lock_before_selecting_match_ids_all() {
        let _serial = TRACE_TEST_LOCK.lock().expect("serialize trace test");
        let (file, _) = repository();
        let repository = MatchDiscoveryRepository::with_trace(file.path(), capture_trace);
        insert_match(&file, 42, GUILD);
        enrich(&repository, 42, GUILD, "manual", None);
        TRACE_STATEMENTS
            .lock()
            .expect("trace statements lock")
            .clear();

        assert_eq!(
            repository
                .wipe_all_enrichments(Some(GUILD))
                .expect("wipe every enrichment"),
            1
        );
        assert_immediate_lock_precedes_match_selection(
            &TRACE_STATEMENTS.lock().expect("trace statements lock"),
        );
    }

    #[test]
    fn stronger_wipe_clears_participants_bans_and_wrapped_facts() {
        let (file, repository) = repository();
        insert_match(&file, 1, GUILD);
        enrich(&repository, 1, GUILD, "auto", Some(1.0));
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute(
                "INSERT INTO match_bans (match_id, discord_id) VALUES (1, 10)",
                [],
            )
            .expect("insert ban");
        connection
            .execute(
                "INSERT INTO wrapped_enrichment_facts (
                     match_id, guild_id, discord_id
                 ) VALUES (1, ?1, 10)",
                [GUILD],
            )
            .expect("insert wrapped fact");
        drop(connection);

        assert!(
            repository
                .wipe_match_enrichment(1, Some(GUILD))
                .expect("wipe complete enrichment")
        );

        let connection = Connection::open(file.path()).expect("inspect fixture");
        let hero_id: Option<i64> = connection
            .query_row(
                "SELECT hero_id FROM match_participants WHERE match_id = 1",
                [],
                |row| row.get(0),
            )
            .expect("participant");
        let bans: i64 = connection
            .query_row("SELECT COUNT(*) FROM match_bans", [], |row| row.get(0))
            .expect("ban count");
        let facts: i64 = connection
            .query_row("SELECT COUNT(*) FROM wrapped_enrichment_facts", [], |row| {
                row.get(0)
            })
            .expect("fact count");
        assert_eq!(hero_id, None);
        assert_eq!(bans, 0);
        assert_eq!(facts, 0);
    }
}
