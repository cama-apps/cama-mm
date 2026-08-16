//! Python-compatible persistence for Camagotchi upbringing and evolution.
//!
//! Python remains the schema-migration authority during the side-by-side
//! rewrite. This repository therefore opens only an existing database, never
//! creates or migrates caller data, and uses `BEGIN IMMEDIATE` for the bounded
//! score write and evolution claim. Activity recording deliberately performs a
//! read-only eligibility preflight before trying to acquire SQLite's single
//! writer lock.

use std::path::{Path, PathBuf};

#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use cama_domain::pet::ADULT_AGE_SECONDS;
use cama_domain::pet_evolution::{
    ActivityRule, EvolutionDecision, InstinctScores, PetActivity, PetInstinct,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

const DAILY_INSTINCT_CAP: i64 = 3;
const DAY_SECONDS: i64 = 86_400;

/// Evolution timestamps assigned when Python creates a new pet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvolutionWindow {
    pub started_at: i64,
    pub due_at: i64,
}

/// Return the exact hatch-to-adulthood score window used by pet adoption.
pub fn adoption_evolution_window(
    hatched_at: i64,
) -> Result<EvolutionWindow, PetEvolutionRepositoryError> {
    let due_at = hatched_at
        .checked_add(ADULT_AGE_SECONDS)
        .ok_or(PetEvolutionRepositoryError::TimestampOverflow)?;
    Ok(EvolutionWindow {
        started_at: hatched_at,
        due_at,
    })
}

#[derive(Debug, Error)]
pub enum PetEvolutionRepositoryError {
    #[error("source_key must not be empty")]
    EmptySourceKey,
    #[error("pet evolution timestamp exceeds SQLite's signed integer range")]
    TimestampOverflow,
    #[error("unknown pet instinct persisted by Python: {0:?}")]
    UnknownInstinct(String),
    #[error("pet-evolution SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Repository for Python's existing `pets` and `pet_evolution_daily` tables.
#[derive(Clone)]
pub struct PetEvolutionRepository {
    path: PathBuf,
    #[cfg(test)]
    write_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    before_write: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl PetEvolutionRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            #[cfg(test)]
            write_attempts: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            before_write: None,
        }
    }

    /// Convert Python's nullable guild convention to its persisted sentinel.
    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Record one eligible activity and return the points actually awarded.
    ///
    /// Ineligible pets, duplicate source keys, once-per-day care repeats, and
    /// already-capped paths all return before a write transaction is opened.
    pub fn record_activity(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        activity: PetActivity,
        source_key: &str,
        occurred_at: i64,
    ) -> Result<i64, PetEvolutionRepositoryError> {
        let source_key = source_key.trim();
        if source_key.is_empty() {
            return Err(PetEvolutionRepositoryError::EmptySourceKey);
        }

        let guild_id = Self::normalize_guild_id(guild_id);
        let rule = cama_domain::pet_evolution::activity_rule(activity);

        // Most actions have no upbringing pet. Match Python by keeping all
        // rejection branches read-only and rechecking after taking the lock.
        let connection = self.connection()?;
        let Some(pet) = eligible_pet(&connection, discord_id, guild_id, occurred_at)? else {
            return Ok(0);
        };
        let Some(preflight_day) = upbringing_day(&pet, occurred_at) else {
            return Ok(0);
        };
        let daily = daily_row(&connection, pet.pet_id, preflight_day, rule.instinct)?;
        if available_points(daily.as_ref(), rule, activity, source_key) <= 0 {
            return Ok(0);
        }
        drop(connection);

        self.note_write_attempt();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let Some(pet) = eligible_pet(&transaction, discord_id, guild_id, occurred_at)? else {
            transaction.commit()?;
            return Ok(0);
        };
        let Some(upbringing_day) = upbringing_day(&pet, occurred_at) else {
            transaction.commit()?;
            return Ok(0);
        };
        let daily = daily_row(&transaction, pet.pet_id, upbringing_day, rule.instinct)?;
        let awarded = available_points(daily.as_ref(), rule, activity, source_key);
        if awarded <= 0 {
            transaction.commit()?;
            return Ok(0);
        }

        if daily.is_none() {
            transaction.execute(
                "INSERT INTO pet_evolution_daily (
                     pet_id, guild_id, upbringing_day, instinct, score,
                     first_activity, first_source_key
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    pet.pet_id,
                    guild_id,
                    upbringing_day,
                    rule.instinct.as_str(),
                    awarded,
                    activity_name(activity),
                    source_key,
                ],
            )?;
        } else {
            transaction.execute(
                "UPDATE pet_evolution_daily
                 SET score = score + ?1,
                     second_activity = ?2,
                     second_source_key = ?3
                 WHERE pet_id = ?4 AND upbringing_day = ?5 AND instinct = ?6",
                params![
                    awarded,
                    activity_name(activity),
                    source_key,
                    pet.pet_id,
                    upbringing_day,
                    rule.instinct.as_str(),
                ],
            )?;
        }

        transaction.commit()?;
        Ok(awarded)
    }

    /// Sum all seven bounded daily rows for each instinct.
    pub fn get_scores(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<InstinctScores, PetEvolutionRepositoryError> {
        let connection = self.connection()?;
        scores(&connection, pet_id, Self::normalize_guild_id(guild_id))
    }

    /// Return a bounded, indexed batch of unresolved pets whose window is due.
    pub fn find_due_refs(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<(i64, i64)>, PetEvolutionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT pet_id, guild_id
             FROM pets
             WHERE died_at IS NULL AND evolved_at IS NULL
               AND evolution_due_at IS NOT NULL
               AND evolution_due_at <= ?1
             ORDER BY evolution_due_at, pet_id
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Resolve the score snapshot and persist the adult calling under one lock.
    pub fn claim_evolution<F>(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
        expected_due_at: i64,
        decide: F,
    ) -> Result<bool, PetEvolutionRepositoryError>
    where
        F: FnOnce(&InstinctScores) -> EvolutionDecision,
    {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let eligible = transaction
            .query_row(
                "SELECT pet_id
                 FROM pets
                 WHERE pet_id = ?1 AND guild_id = ?2
                   AND evolved_at IS NULL
                   AND evolution_due_at = ?3
                   AND (died_at IS NULL OR died_at > evolution_due_at)",
                params![pet_id, guild_id, expected_due_at],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if eligible.is_none() {
            transaction.commit()?;
            return Ok(false);
        }

        let decision = decide(&scores(&transaction, pet_id, guild_id)?);
        let changed = transaction.execute(
            "UPDATE pets
             SET evolved_at = evolution_due_at,
                 evolution_calling = ?1,
                 evolution_primary = ?2,
                 evolution_secondary = ?3
             WHERE pet_id = ?4 AND guild_id = ?5
               AND evolved_at IS NULL
               AND evolution_due_at = ?6
               AND (died_at IS NULL OR died_at > evolution_due_at)",
            params![
                decision.calling.as_str(),
                decision.primary.map(PetInstinct::as_str),
                decision.secondary.map(PetInstinct::as_str),
                pet_id,
                guild_id,
                expected_due_at,
            ],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Return evolved-but-unannounced pets oldest first, with a hard limit.
    pub fn find_unannounced_refs(
        &self,
        limit: i64,
    ) -> Result<Vec<(i64, i64)>, PetEvolutionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT pet_id, guild_id
             FROM pets
             WHERE evolved_at IS NOT NULL
               AND evolution_announced_at IS NULL
             ORDER BY evolved_at, pet_id
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Return the distinct adult callings previously raised by one user.
    pub fn callings_raised(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<String>, PetEvolutionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT evolution_calling
             FROM pets
             WHERE discord_id = ?1 AND guild_id = ?2
               AND evolved_at IS NOT NULL
               AND evolution_calling IS NOT NULL
             ORDER BY evolution_calling",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Mark an evolved pet's notice delivered, scoped to its guild.
    pub fn mark_announced(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), PetEvolutionRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE pets
             SET evolution_announced_at = ?1
             WHERE pet_id = ?2 AND guild_id = ?3
               AND evolved_at IS NOT NULL",
            params![announced_at, pet_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(())
    }

    #[cfg(test)]
    fn note_write_attempt(&self) {
        self.write_attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(before_write) = &self.before_write {
            before_write();
        }
    }

    #[cfg(not(test))]
    const fn note_write_attempt(&self) {}

    #[cfg(test)]
    fn write_attempt_count(&self) -> usize {
        self.write_attempts.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn set_before_write(&mut self, before_write: Arc<dyn Fn() + Send + Sync>) {
        self.before_write = Some(before_write);
    }
}

#[derive(Clone, Copy, Debug)]
struct EligiblePet {
    pet_id: i64,
    evolution_started_at: i64,
}

#[derive(Clone, Debug)]
struct DailyRow {
    score: i64,
    first_activity: String,
    first_source_key: String,
    second_activity: Option<String>,
    second_source_key: Option<String>,
}

fn eligible_pet(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    occurred_at: i64,
) -> Result<Option<EligiblePet>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT pet_id, evolution_started_at
             FROM pets
             WHERE discord_id = ?1 AND guild_id = ?2
               AND died_at IS NULL AND evolved_at IS NULL
               AND evolution_started_at IS NOT NULL
               AND evolution_due_at IS NOT NULL
               AND evolution_started_at <= ?3
               AND evolution_due_at > ?3",
            params![discord_id, guild_id, occurred_at],
            |row| {
                Ok(EligiblePet {
                    pet_id: row.get(0)?,
                    evolution_started_at: row.get(1)?,
                })
            },
        )
        .optional()
}

fn upbringing_day(pet: &EligiblePet, occurred_at: i64) -> Option<i64> {
    let day = occurred_at
        .checked_sub(pet.evolution_started_at)?
        .checked_div(DAY_SECONDS)?;
    (0..=6).contains(&day).then_some(day)
}

fn daily_row(
    connection: &Connection,
    pet_id: i64,
    upbringing_day: i64,
    instinct: PetInstinct,
) -> Result<Option<DailyRow>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT score, first_activity, first_source_key,
                    second_activity, second_source_key
             FROM pet_evolution_daily
             WHERE pet_id = ?1 AND upbringing_day = ?2 AND instinct = ?3",
            params![pet_id, upbringing_day, instinct.as_str()],
            |row| {
                Ok(DailyRow {
                    score: row.get(0)?,
                    first_activity: row.get(1)?,
                    first_source_key: row.get(2)?,
                    second_activity: row.get(3)?,
                    second_source_key: row.get(4)?,
                })
            },
        )
        .optional()
}

fn available_points(
    daily: Option<&DailyRow>,
    rule: ActivityRule,
    activity: PetActivity,
    source_key: &str,
) -> i64 {
    let Some(daily) = daily else {
        return rule.points.min(DAILY_INSTINCT_CAP);
    };
    if daily.first_source_key == source_key
        || daily.second_source_key.as_deref() == Some(source_key)
    {
        return 0;
    }
    let activity = activity_name(activity);
    if rule.once_per_day
        && (daily.first_activity == activity || daily.second_activity.as_deref() == Some(activity))
    {
        return 0;
    }
    rule.points.min(DAILY_INSTINCT_CAP - daily.score)
}

fn scores(
    connection: &Connection,
    pet_id: i64,
    guild_id: i64,
) -> Result<InstinctScores, PetEvolutionRepositoryError> {
    let mut totals = PetInstinct::ALL
        .into_iter()
        .map(|instinct| (instinct, 0))
        .collect::<InstinctScores>();
    let mut statement = connection.prepare(
        "SELECT instinct, SUM(score) AS total
         FROM pet_evolution_daily
         WHERE pet_id = ?1 AND guild_id = ?2
         GROUP BY instinct",
    )?;
    let mut rows = statement.query(params![pet_id, guild_id])?;
    while let Some(row) = rows.next()? {
        let raw_instinct = row.get::<_, String>(0)?;
        let instinct = parse_instinct(&raw_instinct)?;
        totals.insert(instinct, row.get::<_, i64>(1)?);
    }
    Ok(totals)
}

fn parse_instinct(value: &str) -> Result<PetInstinct, PetEvolutionRepositoryError> {
    match value {
        "delving" => Ok(PetInstinct::Delving),
        "competition" => Ok(PetInstinct::Competition),
        "fortune" => Ok(PetInstinct::Fortune),
        "fellowship" => Ok(PetInstinct::Fellowship),
        "wisdom" => Ok(PetInstinct::Wisdom),
        _ => Err(PetEvolutionRepositoryError::UnknownInstinct(
            value.to_owned(),
        )),
    }
}

const fn activity_name(activity: PetActivity) -> &'static str {
    match activity {
        PetActivity::DigCompleted => "dig_completed",
        PetActivity::MatchRecorded => "match_recorded",
        PetActivity::PetBrawlCompleted => "pet_brawl_completed",
        PetActivity::DuelCompleted => "duel_completed",
        PetActivity::WheelSpun => "wheel_spun",
        PetActivity::BetPlaced => "bet_placed",
        PetActivity::PredictionTraded => "prediction_traded",
        PetActivity::PetCared => "pet_cared",
        PetActivity::TipSent => "tip_sent",
        PetActivity::TriviaCompleted => "trivia_completed",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;

    use cama_domain::pet::ADULT_AGE_SECONDS;
    use cama_domain::pet_evolution::{PetActivity, PetCalling, PetInstinct, resolve_evolution};
    use rusqlite::{Connection, OptionalExtension, params};
    use tempfile::NamedTempFile;

    use super::{PetEvolutionRepository, adoption_evolution_window};

    const T0: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const TEST_GUILD_ID: i64 = 987_654_321;

    struct Fixture {
        file: NamedTempFile,
        repository: PetEvolutionRepository,
    }

    #[derive(Clone, Copy)]
    struct PetSeed {
        discord_id: i64,
        guild_id: i64,
        start: i64,
        due: i64,
        died_at: Option<i64>,
        evolved_at: Option<i64>,
    }

    impl Default for PetSeed {
        fn default() -> Self {
            Self {
                discord_id: 100,
                guild_id: TEST_GUILD_ID,
                start: T0,
                due: T0 + 7 * DAY,
                died_at: None,
                evolved_at: None,
            }
        }
    }

    impl Fixture {
        fn new() -> Self {
            let file = NamedTempFile::new().expect("create temporary SQLite file");
            let connection = Connection::open(file.path()).expect("open temporary SQLite file");
            connection
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE pets (
                         pet_id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                         discord_id             INTEGER NOT NULL,
                         guild_id               INTEGER NOT NULL DEFAULT 0,
                         name                   TEXT NOT NULL,
                         species                TEXT NOT NULL,
                         adopted_at             INTEGER NOT NULL,
                         hatched_at             INTEGER NOT NULL,
                         adopt_fee              INTEGER NOT NULL,
                         last_fed_at            INTEGER NOT NULL,
                         hunger_at_last_fed     INTEGER NOT NULL,
                         died_at                INTEGER,
                         death_cause            TEXT,
                         evolution_started_at   INTEGER,
                         evolution_due_at       INTEGER,
                         evolved_at             INTEGER,
                         evolution_calling      TEXT,
                         evolution_primary      TEXT,
                         evolution_secondary    TEXT,
                         evolution_announced_at INTEGER
                     );
                     CREATE UNIQUE INDEX idx_pets_one_living
                         ON pets(discord_id, guild_id) WHERE died_at IS NULL;
                     CREATE TABLE pet_evolution_daily (
                         pet_id            INTEGER NOT NULL,
                         guild_id          INTEGER NOT NULL,
                         upbringing_day    INTEGER NOT NULL
                             CHECK (upbringing_day BETWEEN 0 AND 6),
                         instinct          TEXT NOT NULL,
                         score             INTEGER NOT NULL CHECK (score BETWEEN 0 AND 3),
                         first_activity    TEXT NOT NULL,
                         first_source_key  TEXT NOT NULL,
                         second_activity   TEXT,
                         second_source_key TEXT,
                         PRIMARY KEY (pet_id, upbringing_day, instinct)
                     );
                     CREATE INDEX idx_pets_evolution_due
                         ON pets(evolution_due_at, pet_id)
                         WHERE died_at IS NULL AND evolved_at IS NULL
                           AND evolution_due_at IS NOT NULL;
                     CREATE INDEX idx_pets_evolution_unannounced
                         ON pets(evolved_at, pet_id)
                         WHERE evolved_at IS NOT NULL
                           AND evolution_announced_at IS NULL;",
                )
                .expect("create Python-compatible pet evolution schema");
            drop(connection);
            let repository = PetEvolutionRepository::new(file.path());
            Self { file, repository }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.file.path()).expect("open fixture database")
        }

        fn insert_pet(&self, seed: PetSeed) -> i64 {
            let connection = self.connection();
            connection
                .execute(
                    "INSERT INTO pets (
                         discord_id, guild_id, name, species, adopted_at, hatched_at,
                         adopt_fee, last_fed_at, hunger_at_last_fed,
                         died_at, death_cause,
                         evolution_started_at, evolution_due_at, evolved_at,
                         evolution_calling
                     ) VALUES (
                         ?1, ?2, 'Blep', 'common_cama', ?3, ?4,
                         20, ?4, 100, ?5, ?6, ?4, ?7, ?8, ?9
                     )",
                    params![
                        seed.discord_id,
                        seed.guild_id,
                        seed.start - DAY,
                        seed.start,
                        seed.died_at,
                        seed.died_at.map(|_| "test death"),
                        seed.due,
                        seed.evolved_at,
                        seed.evolved_at.map(|_| "delver"),
                    ],
                )
                .expect("insert pet fixture");
            connection.last_insert_rowid()
        }

        fn score(&self, activity: PetActivity, source_key: &str) -> i64 {
            self.score_at(activity, source_key, 100, Some(TEST_GUILD_ID), T0 + 100)
        }

        fn score_at(
            &self,
            activity: PetActivity,
            source_key: &str,
            discord_id: i64,
            guild_id: Option<i64>,
            occurred_at: i64,
        ) -> i64 {
            self.repository
                .record_activity(discord_id, guild_id, activity, source_key, occurred_at)
                .expect("record evolution activity")
        }
    }

    mod bounded_daily_scores {
        use super::*;

        #[test]
        fn test_first_action_awards_two_second_fills_cap_and_duplicates_are_noops() {
            let fixture = Fixture::new();
            let pet_id = fixture.insert_pet(PetSeed::default());

            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:1"), 2);
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:1"), 0);
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:2"), 1);
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:3"), 0);

            let scores = fixture
                .repository
                .get_scores(pet_id, Some(TEST_GUILD_ID))
                .unwrap();
            assert_eq!(scores[&PetInstinct::Delving], 3);
            assert_eq!(scores[&PetInstinct::Competition], 0);
            assert_eq!(scores[&PetInstinct::Fortune], 0);
            assert_eq!(scores[&PetInstinct::Fellowship], 0);
            assert_eq!(scores[&PetInstinct::Wisdom], 0);

            let row = fixture
                .connection()
                .query_row(
                    "SELECT score, first_source_key, second_source_key
                     FROM pet_evolution_daily WHERE pet_id = ?1",
                    [pet_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(row, (3, "dig:1".to_owned(), Some("dig:2".to_owned())));
        }

        #[test]
        fn test_routine_care_scores_once_but_a_tip_can_fill_the_daily_path_cap() {
            let fixture = Fixture::new();
            let pet_id = fixture.insert_pet(PetSeed::default());

            assert_eq!(fixture.score(PetActivity::PetCared, "feed:1"), 1);
            assert_eq!(fixture.score(PetActivity::PetCared, "feed:2"), 0);
            assert_eq!(fixture.score(PetActivity::TipSent, "tip:1"), 2);

            let scores = fixture
                .repository
                .get_scores(pet_id, Some(TEST_GUILD_ID))
                .unwrap();
            assert_eq!(scores[&PetInstinct::Fellowship], 3);
        }

        #[test]
        fn test_rows_stay_bounded_when_the_window_intersects_eight_calendar_dates() {
            let fixture = Fixture::new();
            let start = T0 + DAY - 60;
            let pet_id = fixture.insert_pet(PetSeed {
                start,
                due: start + 7 * DAY,
                ..PetSeed::default()
            });
            let activities = [
                (PetInstinct::Delving, PetActivity::DigCompleted),
                (PetInstinct::Competition, PetActivity::MatchRecorded),
                (PetInstinct::Fortune, PetActivity::WheelSpun),
                (PetInstinct::Fellowship, PetActivity::TipSent),
                (PetInstinct::Wisdom, PetActivity::TriviaCompleted),
            ];
            let mut instants = vec![start + 1, start + 61];
            instants.extend((1..7).map(|day| start + day * DAY + 100));
            assert_eq!(instants.len(), 8);

            for (instant_index, occurred_at) in instants.into_iter().enumerate() {
                for (instinct, activity) in activities {
                    for attempt in 0..3 {
                        let source = format!("{}:{instant_index}:{attempt}", instinct.as_str());
                        fixture.score_at(activity, &source, 100, Some(TEST_GUILD_ID), occurred_at);
                    }
                }
            }

            let row_count = fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM pet_evolution_daily WHERE pet_id = ?1",
                    [pet_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            assert_eq!(row_count, 35);

            let invalid_rows = fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM pet_evolution_daily
                     WHERE pet_id = ?1 AND (upbringing_day NOT BETWEEN 0 AND 6
                                            OR score NOT BETWEEN 0 AND 3)",
                    [pet_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            assert_eq!(invalid_rows, 0);
        }
    }

    mod evolution_windows {
        use super::*;

        #[test]
        fn test_new_adoption_scores_from_hatch_until_normal_adulthood() {
            let hatched_at = T0 + DAY;
            let window = adoption_evolution_window(hatched_at).unwrap();
            assert_eq!(window.started_at, hatched_at);
            assert_eq!(window.due_at, hatched_at + ADULT_AGE_SECONDS);

            let fixture = Fixture::new();
            let pet_id = fixture.insert_pet(PetSeed {
                start: window.started_at,
                due: window.due_at,
                ..PetSeed::default()
            });
            let stored = fixture
                .connection()
                .query_row(
                    "SELECT evolution_started_at, evolution_due_at, evolved_at
                     FROM pets WHERE pet_id = ?1",
                    [pet_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(stored, (hatched_at, hatched_at + 7 * DAY, None));
        }
    }

    mod eligibility_and_isolation {
        use super::*;

        #[test]
        fn test_users_without_an_upbringing_pet_do_not_take_a_write_lock() {
            let fixture = Fixture::new();
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:no-pet"), 0);
            assert_eq!(fixture.repository.write_attempt_count(), 0);
        }

        #[test]
        fn test_read_only_preflight_rejects_duplicate_source_key() {
            let fixture = Fixture::new();
            fixture.insert_pet(PetSeed::default());
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:duplicate"), 2);
            let write_attempts = fixture.repository.write_attempt_count();
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:duplicate"), 0);
            assert_eq!(fixture.repository.write_attempt_count(), write_attempts);
        }

        #[test]
        fn test_read_only_preflight_rejects_second_daily_care() {
            let fixture = Fixture::new();
            fixture.insert_pet(PetSeed::default());
            assert_eq!(fixture.score(PetActivity::PetCared, "care:first"), 1);
            let write_attempts = fixture.repository.write_attempt_count();
            assert_eq!(fixture.score(PetActivity::PetCared, "care:second"), 0);
            assert_eq!(fixture.repository.write_attempt_count(), write_attempts);
        }

        #[test]
        fn test_read_only_preflight_rejects_an_already_capped_path() {
            let fixture = Fixture::new();
            fixture.insert_pet(PetSeed::default());
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:1"), 2);
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:2"), 1);
            let write_attempts = fixture.repository.write_attempt_count();
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:3"), 0);
            assert_eq!(fixture.repository.write_attempt_count(), write_attempts);
        }

        #[test]
        fn test_guild_scoping_selects_only_the_pet_in_that_guild() {
            let fixture = Fixture::new();
            let pet_a = fixture.insert_pet(PetSeed {
                guild_id: 111,
                ..PetSeed::default()
            });
            let pet_b = fixture.insert_pet(PetSeed {
                guild_id: 222,
                ..PetSeed::default()
            });

            assert_eq!(
                fixture.score_at(PetActivity::DigCompleted, "dig:1", 100, Some(111), T0 + 100,),
                2
            );
            assert_eq!(
                fixture.repository.get_scores(pet_a, Some(111)).unwrap()[&PetInstinct::Delving],
                2
            );
            assert_eq!(
                fixture.repository.get_scores(pet_b, Some(222)).unwrap()[&PetInstinct::Delving],
                0
            );
        }

        #[test]
        fn test_event_one_second_before_window_is_ignored() {
            assert_outside_window_is_ignored(PetSeed::default(), T0 - 1);
        }

        #[test]
        fn test_event_at_exact_due_timestamp_is_ignored() {
            assert_outside_window_is_ignored(PetSeed::default(), T0 + 7 * DAY);
        }

        #[test]
        fn test_event_for_dead_pet_is_ignored() {
            assert_outside_window_is_ignored(
                PetSeed {
                    died_at: Some(T0 + 50),
                    ..PetSeed::default()
                },
                T0 + 100,
            );
        }

        #[test]
        fn test_event_for_evolved_pet_is_ignored() {
            assert_outside_window_is_ignored(
                PetSeed {
                    evolved_at: Some(T0 + 50),
                    ..PetSeed::default()
                },
                T0 + 100,
            );
        }

        fn assert_outside_window_is_ignored(seed: PetSeed, occurred_at: i64) {
            let fixture = Fixture::new();
            let pet_id = fixture.insert_pet(seed);
            assert_eq!(
                fixture.score_at(
                    PetActivity::DigCompleted,
                    "dig:outside",
                    100,
                    Some(TEST_GUILD_ID),
                    occurred_at,
                ),
                0
            );
            assert_eq!(
                fixture
                    .repository
                    .get_scores(pet_id, Some(TEST_GUILD_ID))
                    .unwrap()[&PetInstinct::Delving],
                0
            );
            assert_eq!(fixture.repository.write_attempt_count(), 0);
        }
    }

    mod atomic_resolution {
        use super::*;

        #[test]
        fn test_activity_cannot_commit_between_score_snapshot_and_calling_claim() {
            let fixture = Fixture::new();
            let due = T0 + 7 * DAY;
            let pet_id = fixture.insert_pet(PetSeed {
                due,
                ..PetSeed::default()
            });
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:1"), 2);
            assert_eq!(fixture.score(PetActivity::DigCompleted, "dig:2"), 1);

            let resolver_repository = fixture.repository.clone();
            let mut writer_repository = PetEvolutionRepository::new(fixture.file.path());
            let (decision_started_tx, decision_started_rx) = mpsc::sync_channel(1);
            let (finish_decision_tx, finish_decision_rx) = mpsc::sync_channel(1);
            let (writer_entered_tx, writer_entered_rx) = mpsc::sync_channel(1);
            writer_repository.set_before_write(Arc::new(move || {
                writer_entered_tx
                    .send(())
                    .expect("signal writer transaction attempt");
            }));

            let resolver = thread::spawn(move || {
                resolver_repository
                    .claim_evolution(pet_id, Some(TEST_GUILD_ID), due, |scores| {
                        decision_started_tx.send(()).expect("signal score snapshot");
                        finish_decision_rx
                            .recv()
                            .expect("wait to complete evolution decision");
                        resolve_evolution(scores, pet_id)
                    })
                    .expect("claim evolution")
            });
            decision_started_rx
                .recv()
                .expect("resolver acquired write transaction");

            let writer = thread::spawn(move || {
                writer_repository
                    .record_activity(
                        100,
                        Some(TEST_GUILD_ID),
                        PetActivity::WheelSpun,
                        "wheel:late",
                        due - 1,
                    )
                    .expect("record racing activity")
            });
            writer_entered_rx
                .recv()
                .expect("writer reached BEGIN IMMEDIATE boundary");
            finish_decision_tx
                .send(())
                .expect("allow resolver to persist calling");

            assert!(resolver.join().expect("resolver thread joins"));
            assert_eq!(writer.join().expect("writer thread joins"), 0);
            let calling = fixture
                .connection()
                .query_row(
                    "SELECT evolution_calling FROM pets WHERE pet_id = ?1",
                    [pet_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap();
            assert_eq!(calling, PetCalling::Delver.as_str());
            assert_eq!(
                fixture
                    .repository
                    .get_scores(pet_id, Some(TEST_GUILD_ID))
                    .unwrap()[&PetInstinct::Fortune],
                0
            );
        }
    }

    #[test]
    fn test_unannounced_evolutions_are_returned_in_bounded_order() {
        let fixture = Fixture::new();
        let mut pet_ids = Vec::new();
        for index in 0..3 {
            pet_ids.push(fixture.insert_pet(PetSeed {
                discord_id: 100 + index,
                evolved_at: Some(T0 + index),
                ..PetSeed::default()
            }));
        }
        let announced = fixture.insert_pet(PetSeed {
            discord_id: 200,
            evolved_at: Some(T0 - 1),
            ..PetSeed::default()
        });
        fixture
            .repository
            .mark_announced(announced, Some(TEST_GUILD_ID), T0 + 10)
            .unwrap();

        assert_eq!(
            fixture.repository.find_unannounced_refs(2).unwrap(),
            [(pet_ids[0], TEST_GUILD_ID), (pet_ids[1], TEST_GUILD_ID)]
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT evolution_announced_at FROM pets WHERE pet_id = ?1",
                    [announced],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()
                .unwrap(),
            Some(Some(T0 + 10))
        );
    }

    #[test]
    fn stronger_repository_invariants_cover_due_claims_calling_history_and_dm_scope() {
        let fixture = Fixture::new();
        let due = T0 + 7 * DAY;
        let dm_pet = fixture.insert_pet(PetSeed {
            discord_id: 777,
            guild_id: 0,
            due,
            ..PetSeed::default()
        });
        assert_eq!(
            fixture.score_at(
                PetActivity::TriviaCompleted,
                " trivia:dm ",
                777,
                None,
                due - 1,
            ),
            2
        );
        assert_eq!(
            fixture.repository.find_due_refs(due, 100).unwrap(),
            [(dm_pet, 0)]
        );
        assert!(
            !fixture
                .repository
                .claim_evolution(dm_pet, None, due + 1, |scores| {
                    resolve_evolution(scores, dm_pet)
                })
                .unwrap()
        );
        assert!(
            fixture
                .repository
                .claim_evolution(dm_pet, None, due, |scores| {
                    resolve_evolution(scores, dm_pet)
                })
                .unwrap()
        );
        assert_eq!(
            fixture.repository.callings_raised(777, None).unwrap(),
            ["wayfarer".to_owned()]
        );
        let stored_source = fixture
            .connection()
            .query_row(
                "SELECT first_source_key FROM pet_evolution_daily WHERE pet_id = ?1",
                [dm_pet],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(stored_source, "trivia:dm");
    }
}
