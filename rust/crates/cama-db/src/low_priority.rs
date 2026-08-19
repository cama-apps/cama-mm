//! Python-compatible access to guild-scoped low-priority matchmaking state.
//!
//! Python remains the only schema-migration authority during the side-by-side
//! rewrite. This repository only opens an existing database and uses the
//! current `low_priority_state` table. Assignment inputs retain Python's
//! dynamic integer distinction so boolean and floating-point values can be
//! rejected before any database write occurs.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, params_from_iter};
use thiserror::Error;

use crate::open_runtime_connection;

pub const LOW_PRIORITY_REQUIRED_WINS: i64 = 3;
pub const LOW_PRIORITY_MIN_WINS: i64 = 1;
pub const LOW_PRIORITY_MAX_WINS: i64 = 20;

/// A Python-shaped value supplied where the repository requires an integer.
///
/// Rust would ordinarily make invalid calls impossible at compile time. The
/// explicit variants are useful at language boundaries because Python rejects
/// `bool` and `float` values even though `bool` is an `int` subclass there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PythonIntegerInput {
    Integer(i64),
    Boolean(bool),
    Float(f64),
}

impl From<i64> for PythonIntegerInput {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<bool> for PythonIntegerInput {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<f64> for PythonIntegerInput {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

/// One persisted row from Python's `low_priority_state` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowPriorityState {
    pub discord_id: i64,
    pub guild_id: i64,
    pub wins_required: i64,
    pub wins_remaining: i64,
    pub start_pending_match_id: Option<i64>,
    pub active: bool,
    pub reason: Option<String>,
    pub set_by: i64,
    pub removed_by: Option<i64>,
    pub removed_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Inputs for assigning or reassigning one player to low priority.
#[derive(Clone, Debug, PartialEq)]
pub struct SetLowPriorityInput {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub set_by: i64,
    pub reason: Option<String>,
    pub wins_required: PythonIntegerInput,
    pub start_pending_match_id: Option<PythonIntegerInput>,
}

impl SetLowPriorityInput {
    /// Construct an assignment with Python's default three required wins and
    /// no pending-match watermark.
    #[must_use]
    pub fn new(
        discord_id: i64,
        guild_id: Option<i64>,
        set_by: i64,
        reason: Option<String>,
    ) -> Self {
        Self {
            discord_id,
            guild_id,
            set_by,
            reason,
            wins_required: PythonIntegerInput::Integer(LOW_PRIORITY_REQUIRED_WINS),
            start_pending_match_id: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum LowPriorityRepositoryError {
    #[error("wins_required must be an integer")]
    InvalidWinsRequiredType,
    #[error("wins_required must be between {LOW_PRIORITY_MIN_WINS} and {LOW_PRIORITY_MAX_WINS}")]
    InvalidWinsRequiredRange,
    #[error("start_pending_match_id must be a non-negative integer or None")]
    InvalidPendingMatchIdType,
    #[error("start_pending_match_id must be a non-negative integer or None")]
    InvalidPendingMatchIdRange,
    #[error("low-priority write succeeded but state was not found")]
    MissingStateAfterWrite,
    #[error("low-priority SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Repository for Python's existing `low_priority_state` table.
#[derive(Clone, Debug)]
pub struct LowPriorityRepository {
    path: PathBuf,
}

impl LowPriorityRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
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

    pub fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LowPriorityState>, LowPriorityRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        Ok(self
            .connection()?
            .query_row(
                "SELECT discord_id, guild_id, wins_required, wins_remaining,
                        start_pending_match_id, active, reason, set_by,
                        removed_by, removed_reason, created_at, updated_at
                 FROM low_priority_state
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                row_to_state,
            )
            .optional()?)
    }

    /// Assign or replace a player's current state in an immediate transaction.
    ///
    /// Validation happens before opening the database, matching Python's
    /// no-write behavior for invalid assignment arguments.
    pub fn set_low_priority(
        &self,
        input: &SetLowPriorityInput,
    ) -> Result<LowPriorityState, LowPriorityRepositoryError> {
        let wins_required = validate_wins_required(input.wins_required)?;
        let start_pending_match_id = validate_pending_match_id(input.start_pending_match_id)?;
        let guild_id = Self::normalize_guild_id(input.guild_id);

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO low_priority_state (
                 discord_id, guild_id, wins_required, wins_remaining,
                 start_pending_match_id, active, reason, set_by, removed_by,
                 removed_reason, created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?3, ?4, 1, ?5, ?6, NULL, NULL,
                     CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 wins_required = excluded.wins_required,
                 wins_remaining = excluded.wins_remaining,
                 start_pending_match_id = excluded.start_pending_match_id,
                 active = 1,
                 reason = excluded.reason,
                 set_by = excluded.set_by,
                 removed_by = NULL,
                 removed_reason = NULL,
                 created_at = CURRENT_TIMESTAMP,
                 updated_at = CURRENT_TIMESTAMP",
            params![
                input.discord_id,
                guild_id,
                wins_required,
                start_pending_match_id,
                input.reason.as_deref(),
                input.set_by,
            ],
        )?;
        let state = transaction
            .query_row(
                "SELECT discord_id, guild_id, wins_required, wins_remaining,
                        start_pending_match_id, active, reason, set_by,
                        removed_by, removed_reason, created_at, updated_at
                 FROM low_priority_state
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![input.discord_id, guild_id],
                row_to_state,
            )
            .optional()?;
        transaction.commit()?;
        state.ok_or(LowPriorityRepositoryError::MissingStateAfterWrite)
    }

    /// Clear an active assignment, retaining the row as audit-visible state.
    pub fn clear_low_priority(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        removed_by: i64,
        reason: Option<&str>,
    ) -> Result<bool, LowPriorityRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let updated = self.connection()?.execute(
            "UPDATE low_priority_state
             SET wins_remaining = 0,
                 active = 0,
                 removed_by = ?1,
                 removed_reason = ?2,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?3 AND guild_id = ?4 AND active = 1",
            params![removed_by, reason, discord_id, guild_id],
        )?;
        Ok(updated > 0)
    }

    /// Return the requested IDs whose assignment remains active in one guild.
    pub fn get_active_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashSet<i64>, LowPriorityRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let placeholders = std::iter::repeat_n("?", discord_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT discord_id
             FROM low_priority_state
             WHERE guild_id = ? AND active = 1 AND wins_remaining > 0
               AND discord_id IN ({placeholders})"
        );
        let mut parameters = Vec::with_capacity(discord_ids.len() + 1);
        parameters.push(Value::Integer(Self::normalize_guild_id(guild_id)));
        parameters.extend(discord_ids.iter().copied().map(Value::Integer));

        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(parameters.iter()), |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Active low-priority Discord IDs for one guild.
    ///
    /// Shaped as a `BTreeSet` so the economy paths that already gate a
    /// per-player tax rate on a membership set can consult low priority the
    /// same way they consult vanity-tax enforcement. Unlike
    /// [`Self::get_active_ids`] this needs no candidate list, because a
    /// settlement learns which players it is paying only once it is already
    /// inside its own transaction.
    pub fn active_taxable_ids(
        &self,
        guild_id: Option<i64>,
    ) -> Result<BTreeSet<i64>, LowPriorityRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id
             FROM low_priority_state
             WHERE guild_id = ?1 AND active = 1 AND wins_remaining > 0",
        )?;
        let rows = statement.query_map([guild_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// List active assignments for a guild in stable player-ID order.
    pub fn list_active(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<LowPriorityState>, LowPriorityRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, guild_id, wins_required, wins_remaining,
                    start_pending_match_id, active, reason, set_by,
                    removed_by, removed_reason, created_at, updated_at
             FROM low_priority_state
             WHERE guild_id = ?1 AND active = 1 AND wins_remaining > 0
             ORDER BY discord_id",
        )?;
        let rows = statement.query_map([guild_id], row_to_state)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn validate_wins_required(input: PythonIntegerInput) -> Result<i64, LowPriorityRepositoryError> {
    let PythonIntegerInput::Integer(value) = input else {
        return Err(LowPriorityRepositoryError::InvalidWinsRequiredType);
    };
    if !(LOW_PRIORITY_MIN_WINS..=LOW_PRIORITY_MAX_WINS).contains(&value) {
        return Err(LowPriorityRepositoryError::InvalidWinsRequiredRange);
    }
    Ok(value)
}

fn validate_pending_match_id(
    input: Option<PythonIntegerInput>,
) -> Result<Option<i64>, LowPriorityRepositoryError> {
    let Some(input) = input else {
        return Ok(None);
    };
    let PythonIntegerInput::Integer(value) = input else {
        return Err(LowPriorityRepositoryError::InvalidPendingMatchIdType);
    };
    if value < 0 {
        return Err(LowPriorityRepositoryError::InvalidPendingMatchIdRange);
    }
    Ok(Some(value))
}

fn row_to_state(row: &Row<'_>) -> Result<LowPriorityState, rusqlite::Error> {
    Ok(LowPriorityState {
        discord_id: row.get(0)?,
        guild_id: row.get(1)?,
        wins_required: row.get(2)?,
        wins_remaining: row.get(3)?,
        start_pending_match_id: row.get(4)?,
        active: python_truthy(row.get_ref(5)?),
        reason: row.get(6)?,
        set_by: row.get(7)?,
        removed_by: row.get(8)?,
        removed_reason: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn python_truthy(value: ValueRef<'_>) -> bool {
    match value {
        ValueRef::Null => false,
        ValueRef::Integer(value) => value != 0,
        ValueRef::Real(value) => value != 0.0,
        ValueRef::Text(value) | ValueRef::Blob(value) => !value.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use super::{
        LOW_PRIORITY_REQUIRED_WINS, LowPriorityRepository, LowPriorityRepositoryError,
        PythonIntegerInput, SetLowPriorityInput,
    };
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    const TEST_GUILD_ID: i64 = 987_654_321;
    const SECONDARY_GUILD_ID: i64 = 987_654_322;

    struct Fixture {
        _file: NamedTempFile,
        repository: LowPriorityRepository,
    }

    impl Fixture {
        fn new() -> Self {
            let file = NamedTempFile::new().expect("create temporary SQLite file");
            let connection = Connection::open(file.path()).expect("open temporary SQLite file");
            connection
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE low_priority_state (
                         discord_id             INTEGER NOT NULL,
                         guild_id               INTEGER NOT NULL DEFAULT 0,
                         wins_required          INTEGER NOT NULL DEFAULT 3
                             CHECK(wins_required BETWEEN 1 AND 20),
                         wins_remaining         INTEGER NOT NULL DEFAULT 3
                             CHECK(wins_remaining BETWEEN 0 AND 20),
                         start_pending_match_id  INTEGER CHECK(start_pending_match_id >= 0),
                         active                 INTEGER NOT NULL DEFAULT 1
                             CHECK(active IN (0, 1)),
                         reason                 TEXT,
                         set_by                 INTEGER NOT NULL,
                         removed_by             INTEGER,
                         removed_reason         TEXT,
                         created_at             TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                         updated_at             TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                         PRIMARY KEY (discord_id, guild_id),
                         CHECK(wins_remaining <= wins_required)
                     );
                     CREATE INDEX idx_low_priority_active_guild
                         ON low_priority_state(guild_id, active, wins_remaining);",
                )
                .expect("create Python-compatible low-priority schema");
            drop(connection);
            let repository = LowPriorityRepository::new(file.path());
            Self {
                _file: file,
                repository,
            }
        }

        fn assignment(&self) -> SetLowPriorityInput {
            SetLowPriorityInput::new(
                101,
                Some(TEST_GUILD_ID),
                901,
                Some("default assignment".to_owned()),
            )
        }
    }

    #[test]
    fn test_schema_creates_low_priority_state() {
        let fixture = Fixture::new();
        let connection = Connection::open(fixture._file.path()).expect("open fixture schema");
        let mut statement = connection
            .prepare("PRAGMA table_info(low_priority_state)")
            .expect("inspect low-priority schema");
        let columns = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
            })
            .expect("query low-priority schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect low-priority schema");
        let names = columns
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "discord_id",
                "guild_id",
                "wins_required",
                "wins_remaining",
                "start_pending_match_id",
                "active",
                "reason",
                "set_by",
                "removed_by",
                "removed_reason",
                "created_at",
                "updated_at",
            ]
        );
        assert_eq!(columns[0].1, 1);
        assert_eq!(columns[1].1, 2);
    }

    #[test]
    fn test_set_clear_and_reset_persist_current_admin_state() {
        let fixture = Fixture::new();
        let mut first = fixture.assignment();
        first.reason = Some("first reason".to_owned());
        let first = fixture.repository.set_low_priority(&first).unwrap();
        assert!(first.active);
        assert_eq!(first.wins_remaining, 3);
        assert_eq!(first.reason.as_deref(), Some("first reason"));
        assert_eq!(first.set_by, 901);

        assert!(
            fixture
                .repository
                .clear_low_priority(101, Some(TEST_GUILD_ID), 902, Some("reviewed"))
                .unwrap()
        );
        let cleared = fixture
            .repository
            .get_state(101, Some(TEST_GUILD_ID))
            .unwrap()
            .expect("retained cleared state");
        assert!(!cleared.active);
        assert_eq!(cleared.wins_remaining, 0);
        assert_eq!(cleared.removed_by, Some(902));
        assert_eq!(cleared.removed_reason.as_deref(), Some("reviewed"));

        let reset = fixture
            .repository
            .set_low_priority(&SetLowPriorityInput::new(
                101,
                Some(TEST_GUILD_ID),
                903,
                None,
            ))
            .unwrap();
        assert!(reset.active);
        assert_eq!(reset.wins_remaining, 3);
        assert_eq!(reset.set_by, 903);
        assert_eq!(reset.removed_by, None);
    }

    #[test]
    fn test_bulk_and_list_queries_are_active_only_and_guild_scoped() {
        let fixture = Fixture::new();
        for discord_id in [101, 102] {
            fixture
                .repository
                .set_low_priority(&SetLowPriorityInput::new(
                    discord_id,
                    Some(TEST_GUILD_ID),
                    901,
                    None,
                ))
                .unwrap();
        }
        fixture
            .repository
            .set_low_priority(&SetLowPriorityInput::new(
                101,
                Some(SECONDARY_GUILD_ID),
                902,
                None,
            ))
            .unwrap();
        fixture
            .repository
            .clear_low_priority(102, Some(TEST_GUILD_ID), 901, None)
            .unwrap();

        assert_eq!(
            fixture
                .repository
                .get_active_ids(&[101, 102, 999], Some(TEST_GUILD_ID))
                .unwrap(),
            HashSet::from([101])
        );
        assert!(
            fixture
                .repository
                .get_active_ids(&[], Some(TEST_GUILD_ID))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .repository
                .list_active(Some(TEST_GUILD_ID))
                .unwrap()
                .into_iter()
                .map(|state| state.discord_id)
                .collect::<Vec<_>>(),
            [101]
        );
        assert!(
            fixture
                .repository
                .get_state(101, Some(SECONDARY_GUILD_ID))
                .unwrap()
                .expect("secondary state")
                .active
        );
    }

    #[test]
    fn test_active_taxable_ids_are_active_only_and_guild_scoped() {
        let fixture = Fixture::new();
        for discord_id in [101, 102] {
            fixture
                .repository
                .set_low_priority(&SetLowPriorityInput::new(
                    discord_id,
                    Some(TEST_GUILD_ID),
                    901,
                    None,
                ))
                .unwrap();
        }
        fixture
            .repository
            .set_low_priority(&SetLowPriorityInput::new(
                777,
                Some(SECONDARY_GUILD_ID),
                902,
                None,
            ))
            .unwrap();
        fixture
            .repository
            .clear_low_priority(102, Some(TEST_GUILD_ID), 901, None)
            .unwrap();

        assert_eq!(
            fixture
                .repository
                .active_taxable_ids(Some(TEST_GUILD_ID))
                .unwrap(),
            BTreeSet::from([101]),
            "a cleared assignment must stop being taxable"
        );
        assert_eq!(
            fixture
                .repository
                .active_taxable_ids(Some(SECONDARY_GUILD_ID))
                .unwrap(),
            BTreeSet::from([777]),
            "taxability must not leak across guilds"
        );
        assert!(
            fixture
                .repository
                .active_taxable_ids(Some(SECONDARY_GUILD_ID + 1))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_active_taxable_ids_drop_a_player_who_has_won_their_way_out() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_low_priority(&SetLowPriorityInput::new(
                101,
                Some(TEST_GUILD_ID),
                901,
                None,
            ))
            .unwrap();
        // Winning out of low priority is recorded by the match path, which
        // drains `wins_remaining` to zero while the row stays active.
        Connection::open(fixture._file.path())
            .expect("open low-priority fixture")
            .execute(
                "UPDATE low_priority_state SET wins_remaining = 0
                 WHERE discord_id = ?1 AND guild_id = ?2",
                [101_i64, TEST_GUILD_ID],
            )
            .expect("drain remaining wins");

        assert!(
            fixture
                .repository
                .active_taxable_ids(Some(TEST_GUILD_ID))
                .unwrap()
                .is_empty(),
            "the tax must stop when the player leaves low priority"
        );
    }

    #[test]
    fn test_assignment_defaults_to_three_wins_without_pending_watermark() {
        let fixture = Fixture::new();
        let input = fixture.assignment();

        let state = fixture.repository.set_low_priority(&input).unwrap();

        assert_eq!(state.wins_required, LOW_PRIORITY_REQUIRED_WINS);
        assert_eq!(state.wins_remaining, LOW_PRIORITY_REQUIRED_WINS);
        assert_eq!(state.start_pending_match_id, None);
        assert_eq!(
            fixture
                .repository
                .get_state(101, Some(TEST_GUILD_ID))
                .unwrap(),
            Some(state)
        );
    }

    #[test]
    fn test_reassignment_replaces_win_count_and_pending_watermark() {
        let fixture = Fixture::new();
        let mut first = fixture.assignment();
        first.reason = Some("first assignment".to_owned());
        first.wins_required = PythonIntegerInput::Integer(5);
        first.start_pending_match_id = Some(PythonIntegerInput::Integer(41));
        fixture.repository.set_low_priority(&first).unwrap();
        assert!(
            fixture
                .repository
                .clear_low_priority(101, Some(TEST_GUILD_ID), 902, Some("reviewed"))
                .unwrap()
        );
        let cleared = fixture
            .repository
            .get_state(101, Some(TEST_GUILD_ID))
            .unwrap()
            .expect("retained cleared state");
        assert!(!cleared.active);
        assert_eq!(cleared.wins_remaining, 0);
        assert_eq!(cleared.removed_by, Some(902));
        assert_eq!(cleared.removed_reason.as_deref(), Some("reviewed"));

        let mut replacement = fixture.assignment();
        replacement.set_by = 903;
        replacement.reason = Some("replacement".to_owned());
        replacement.wins_required = PythonIntegerInput::Integer(20);
        replacement.start_pending_match_id = Some(PythonIntegerInput::Integer(84));
        let replaced = fixture.repository.set_low_priority(&replacement).unwrap();

        assert!(replaced.active);
        assert_eq!(replaced.wins_required, 20);
        assert_eq!(replaced.wins_remaining, 20);
        assert_eq!(replaced.start_pending_match_id, Some(84));
        assert_eq!(replaced.reason.as_deref(), Some("replacement"));
        assert_eq!(replaced.set_by, 903);
        assert_eq!(replaced.removed_by, None);
        assert_eq!(replaced.removed_reason, None);
        assert_eq!(
            fixture
                .repository
                .list_active(Some(TEST_GUILD_ID))
                .unwrap()
                .as_slice(),
            std::slice::from_ref(&replaced)
        );

        let mut other_guild = SetLowPriorityInput::new(101, Some(SECONDARY_GUILD_ID), 904, None);
        other_guild.wins_required = PythonIntegerInput::Integer(2);
        fixture.repository.set_low_priority(&other_guild).unwrap();
        assert_eq!(
            fixture
                .repository
                .get_active_ids(&[101, 101, 999], Some(TEST_GUILD_ID))
                .unwrap(),
            [101].into_iter().collect()
        );
        assert_eq!(
            fixture.repository.list_active(Some(TEST_GUILD_ID)).unwrap(),
            [replaced]
        );
    }

    fn assert_invalid_required_wins(input: PythonIntegerInput, expected_type_error: bool) {
        let fixture = Fixture::new();
        let mut assignment = fixture.assignment();
        assignment.wins_required = input;
        assignment.reason = None;

        let error = fixture
            .repository
            .set_low_priority(&assignment)
            .expect_err("invalid wins_required must fail");
        if expected_type_error {
            assert!(matches!(
                error,
                LowPriorityRepositoryError::InvalidWinsRequiredType
            ));
        } else {
            assert!(matches!(
                error,
                LowPriorityRepositoryError::InvalidWinsRequiredRange
            ));
        }
        assert_eq!(
            fixture
                .repository
                .get_state(101, Some(TEST_GUILD_ID))
                .unwrap(),
            None
        );
    }

    #[test]
    fn test_assignment_rejects_invalid_required_wins_0() {
        assert_invalid_required_wins(PythonIntegerInput::Integer(0), false);
    }

    #[test]
    fn test_assignment_rejects_invalid_required_wins_21() {
        assert_invalid_required_wins(PythonIntegerInput::Integer(21), false);
    }

    #[test]
    fn test_assignment_rejects_invalid_required_wins_negative_1() {
        assert_invalid_required_wins(PythonIntegerInput::Integer(-1), false);
    }

    #[test]
    fn test_assignment_rejects_invalid_required_wins_true() {
        assert_invalid_required_wins(PythonIntegerInput::Boolean(true), true);
    }

    #[test]
    fn test_assignment_rejects_invalid_required_wins_1_5() {
        assert_invalid_required_wins(PythonIntegerInput::Float(1.5), true);
    }

    fn assert_invalid_pending_match_id(input: PythonIntegerInput, expected_type_error: bool) {
        let fixture = Fixture::new();
        let mut assignment = fixture.assignment();
        assignment.start_pending_match_id = Some(input);
        assignment.reason = None;

        let error = fixture
            .repository
            .set_low_priority(&assignment)
            .expect_err("invalid start_pending_match_id must fail");
        if expected_type_error {
            assert!(matches!(
                error,
                LowPriorityRepositoryError::InvalidPendingMatchIdType
            ));
        } else {
            assert!(matches!(
                error,
                LowPriorityRepositoryError::InvalidPendingMatchIdRange
            ));
        }
        assert_eq!(
            fixture
                .repository
                .get_state(101, Some(TEST_GUILD_ID))
                .unwrap(),
            None
        );
    }

    #[test]
    fn test_assignment_rejects_invalid_pending_match_watermark_negative_1() {
        assert_invalid_pending_match_id(PythonIntegerInput::Integer(-1), false);
    }

    #[test]
    fn test_assignment_rejects_invalid_pending_match_watermark_true() {
        assert_invalid_pending_match_id(PythonIntegerInput::Boolean(true), true);
    }

    #[test]
    fn test_assignment_rejects_invalid_pending_match_watermark_1_5() {
        assert_invalid_pending_match_id(PythonIntegerInput::Float(1.5), true);
    }
}
