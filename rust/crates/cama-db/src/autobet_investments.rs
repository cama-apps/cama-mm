//! Existing-schema persistence for Python's automatic investment positions.
//!
//! The betting command treats the configured positions as durable policy.  A
//! position replacement and the 50% aggregate cap therefore need one SQLite
//! transaction, otherwise two concurrent `/economy invest set` calls can both
//! pass validation and leave an invalid portfolio.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutobetInvestment {
    pub guild_id: i64,
    pub investor_id: i64,
    pub target_id: i64,
    pub direction: String,
    pub percentage: i64,
}

#[derive(Debug, Error)]
pub enum AutobetInvestmentRepositoryError {
    #[error("Investment direction must be 'long' or 'short'.")]
    InvalidDirection,
    #[error("Investment percentage must be between 1 and 10.")]
    InvalidPercentage,
    #[error("Total configured investment percentage cannot exceed 50%.")]
    PercentageCap,
    #[error("investment arithmetic overflowed")]
    Overflow,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct AutobetInvestmentRepository {
    path: PathBuf,
}

impl AutobetInvestmentRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

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

    /// Insert or replace one position while enforcing Python's 50% cap.
    ///
    /// The returned value is the investor's new configured total in the
    /// normalized guild.
    pub fn set(
        &self,
        guild_id: Option<i64>,
        investor_id: i64,
        target_id: i64,
        direction: &str,
        percentage: i64,
    ) -> Result<i64, AutobetInvestmentRepositoryError> {
        if !matches!(direction, "long" | "short") {
            return Err(AutobetInvestmentRepositoryError::InvalidDirection);
        }
        if !(1..=10).contains(&percentage) {
            return Err(AutobetInvestmentRepositoryError::InvalidPercentage);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT percentage FROM autobet_investments
                 WHERE guild_id = ?1 AND investor_id = ?2 AND target_id = ?3",
                params![guild_id, investor_id, target_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let current_total = transaction.query_row(
            "SELECT COALESCE(SUM(percentage), 0) FROM autobet_investments
             WHERE guild_id = ?1 AND investor_id = ?2",
            params![guild_id, investor_id],
            |row| row.get::<_, i64>(0),
        )?;
        let total = current_total
            .checked_sub(existing)
            .and_then(|value| value.checked_add(percentage))
            .ok_or(AutobetInvestmentRepositoryError::Overflow)?;
        if total > 50 {
            return Err(AutobetInvestmentRepositoryError::PercentageCap);
        }
        transaction.execute(
            "INSERT INTO autobet_investments
                 (guild_id, investor_id, target_id, direction, percentage,
                  created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             ON CONFLICT(guild_id, investor_id, target_id) DO UPDATE SET
                 direction = excluded.direction,
                 percentage = excluded.percentage,
                 updated_at = CURRENT_TIMESTAMP",
            params![guild_id, investor_id, target_id, direction, percentage],
        )?;
        transaction.commit()?;
        Ok(total)
    }

    pub fn remove(
        &self,
        guild_id: Option<i64>,
        investor_id: i64,
        target_id: i64,
    ) -> Result<bool, AutobetInvestmentRepositoryError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM autobet_investments
             WHERE guild_id = ?1 AND investor_id = ?2 AND target_id = ?3",
            params![Self::normalize_guild_id(guild_id), investor_id, target_id],
        )?;
        Ok(changed == 1)
    }

    pub fn list(
        &self,
        guild_id: Option<i64>,
        investor_id: i64,
    ) -> Result<Vec<AutobetInvestment>, AutobetInvestmentRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT guild_id, investor_id, target_id, direction, percentage
             FROM autobet_investments
             WHERE guild_id = ?1 AND investor_id = ?2
             ORDER BY target_id ASC",
        )?;
        let rows = statement.query_map(
            params![Self::normalize_guild_id(guild_id), investor_id],
            |row| {
                Ok(AutobetInvestment {
                    guild_id: row.get(0)?,
                    investor_id: row.get(1)?,
                    target_id: row.get(2)?,
                    direction: row.get(3)?,
                    percentage: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn for_targets(
        &self,
        guild_id: Option<i64>,
        target_ids: &[i64],
    ) -> Result<Vec<AutobetInvestment>, AutobetInvestmentRepositoryError> {
        if target_ids.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let placeholders = std::iter::repeat_n("?", target_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT guild_id, investor_id, target_id, direction, percentage
             FROM autobet_investments
             WHERE guild_id = ?1 AND target_id IN ({placeholders})
             ORDER BY investor_id ASC, target_id ASC"
        );
        let mut values = Vec::with_capacity(target_ids.len() + 1);
        values.push(rusqlite::types::Value::Integer(Self::normalize_guild_id(
            guild_id,
        )));
        values.extend(
            target_ids
                .iter()
                .copied()
                .map(rusqlite::types::Value::Integer),
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
            Ok(AutobetInvestment {
                guild_id: row.get(0)?,
                investor_id: row.get(1)?,
                target_id: row.get(2)?,
                direction: row.get(3)?,
                percentage: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::copy_migrated_database;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::NamedTempFile;

    #[test]
    fn set_replaces_position_without_double_counting_and_enforces_cap() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = AutobetInvestmentRepository::new(database.path());
        assert_eq!(repository.set(Some(9), 1, 10, "long", 10).unwrap(), 10);
        assert_eq!(repository.set(Some(9), 1, 10, "short", 1).unwrap(), 1);
        assert_eq!(repository.set(Some(9), 1, 11, "long", 10).unwrap(), 11);
        assert_eq!(repository.set(Some(9), 1, 12, "long", 10).unwrap(), 21);
        assert_eq!(repository.set(Some(9), 1, 13, "long", 10).unwrap(), 31);
        assert_eq!(repository.set(Some(9), 1, 14, "long", 10).unwrap(), 41);
        assert!(matches!(
            repository.set(Some(9), 1, 15, "long", 10),
            Err(AutobetInvestmentRepositoryError::PercentageCap)
        ));
        assert_eq!(repository.list(Some(9), 1).unwrap().len(), 5);
    }

    #[test]
    fn remove_and_target_lookup_are_guild_scoped() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = AutobetInvestmentRepository::new(database.path());
        repository.set(Some(1), 2, 20, "long", 5).unwrap();
        repository.set(Some(2), 3, 20, "short", 6).unwrap();
        assert_eq!(repository.for_targets(Some(1), &[20]).unwrap().len(), 1);
        assert!(repository.remove(Some(1), 2, 20).unwrap());
        assert!(!repository.remove(Some(1), 2, 20).unwrap());
    }

    #[test]
    fn set_enforces_total_percentage_cap_without_partial_write() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = AutobetInvestmentRepository::new(database.path());
        for target_id in 201..=205 {
            repository
                .set(Some(9), 1, target_id, "long", 10)
                .expect("initial position");
        }
        assert!(matches!(
            repository.set(Some(9), 1, 206, "short", 1),
            Err(AutobetInvestmentRepositoryError::PercentageCap)
        ));
        let positions = repository.list(Some(9), 1).expect("positions");
        assert_eq!(positions.len(), 5);
        assert_eq!(
            positions
                .iter()
                .map(|position| position.percentage)
                .sum::<i64>(),
            50
        );
    }

    #[test]
    fn concurrent_position_updates_preserve_the_total_cap() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = AutobetInvestmentRepository::new(database.path());
        for target_id in 301..=304 {
            repository
                .set(Some(9), 1, target_id, "long", 10)
                .expect("initial position");
        }
        let barrier = Arc::new(Barrier::new(2));
        let handles = [305_i64, 306_i64]
            .into_iter()
            .map(|target_id| {
                let repository = repository.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository.set(Some(9), 1, target_id, "short", 10)
                })
            })
            .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().expect("investment worker"))
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(outcomes.iter().filter(|result| result.is_err()).count(), 1);
        let positions = repository.list(Some(9), 1).expect("positions after race");
        assert_eq!(
            positions
                .iter()
                .map(|position| position.percentage)
                .sum::<i64>(),
            50
        );
    }

    #[test]
    fn autobet_attribution_schema_has_profile_columns_and_index() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("migrated schema");
        let connection = Connection::open(database.path()).expect("schema connection");
        let columns = connection
            .prepare("PRAGMA table_info(bets)")
            .expect("bets columns")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query bets columns")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect bets columns");
        assert!(
            columns
                .iter()
                .any(|column| column == "investment_target_id")
        );
        assert!(
            columns
                .iter()
                .any(|column| column == "investment_direction")
        );
        let indexes = connection
            .prepare("PRAGMA index_list(bets)")
            .expect("bets indexes")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query bets indexes")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect bets indexes");
        assert!(
            indexes
                .iter()
                .any(|index| index == "idx_bets_autobet_profile")
        );
    }
}
