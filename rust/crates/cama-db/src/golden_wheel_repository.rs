//! Existing-schema reads and spin persistence used by Golden Wheel policy.
//!
//! Python remains the schema and migration authority. Production methods open
//! only the configured existing database through [`crate::open_runtime_connection`].
//! Test-only fixtures create their own disposable schema.

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use thiserror::Error;

use crate::open_runtime_connection;
use crate::wheel_spin_repository::{
    NewWheelSpin, WheelSpinRecord, WheelSpinRepository, WheelSpinRepositoryError,
};

#[derive(Clone, Debug, PartialEq)]
pub struct GoldenBalanceRow {
    pub discord_id: i64,
    pub guild_id: i64,
    pub username: String,
    pub balance: i64,
    pub wins: i64,
    pub glicko_rating: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenWheelColumnInfo {
    pub name: String,
    pub not_null: bool,
    pub default_value: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GoldenSpinLog {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub result: i64,
    pub spin_time: i64,
    pub is_golden: bool,
}

impl GoldenSpinLog {
    #[must_use]
    pub const fn legacy(
        discord_id: i64,
        guild_id: Option<i64>,
        result: i64,
        spin_time: i64,
    ) -> Self {
        Self {
            discord_id,
            guild_id,
            result,
            spin_time,
            is_golden: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum GoldenWheelRepositoryError {
    #[error("minimum balance must be non-negative: {0}")]
    InvalidMinimumBalance(i64),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    WheelSpin(#[from] WheelSpinRepositoryError),
}

#[derive(Clone, Debug)]
pub struct GoldenWheelRepository {
    path: PathBuf,
}

impl GoldenWheelRepository {
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

    /// Return the poorest solvent players in deterministic Python order.
    pub fn get_leaderboard_bottom(
        &self,
        guild_id: Option<i64>,
        limit: usize,
        min_balance: i64,
    ) -> Result<Vec<GoldenBalanceRow>, GoldenWheelRepositoryError> {
        if min_balance < 0 {
            return Err(GoldenWheelRepositoryError::InvalidMinimumBalance(
                min_balance,
            ));
        }
        let connection = open_runtime_connection(&self.path)?;
        let sql_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = connection.prepare(
            "SELECT discord_id, guild_id, COALESCE(discord_username, ''),
                    COALESCE(jopacoin_balance, 0), COALESCE(wins, 0), glicko_rating
               FROM players
              WHERE guild_id = ?1 AND COALESCE(jopacoin_balance, 0) >= ?2
              ORDER BY COALESCE(jopacoin_balance, 0) ASC,
                       COALESCE(wins, 0) ASC,
                       COALESCE(glicko_rating, 0) ASC,
                       discord_id DESC
              LIMIT ?3",
        )?;
        let rows = statement.query_map(
            (Self::normalize_guild_id(guild_id), min_balance, sql_limit),
            |row| {
                Ok(GoldenBalanceRow {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    username: row.get(2)?,
                    balance: row.get(3)?,
                    wins: row.get(4)?,
                    glicko_rating: row.get(5)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Sum only positive liquid balances for one normalized guild.
    pub fn get_total_positive_balance(
        &self,
        guild_id: Option<i64>,
    ) -> Result<i64, GoldenWheelRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT COALESCE(SUM(jopacoin_balance), 0)
                   FROM players
                  WHERE guild_id = ?1 AND COALESCE(jopacoin_balance, 0) > 0",
                [Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Persist a spin through the existing wheel-spins adapter.
    pub fn log_wheel_spin(&self, spin: GoldenSpinLog) -> Result<i64, GoldenWheelRepositoryError> {
        let mut record =
            NewWheelSpin::legacy(spin.discord_id, spin.guild_id, spin.result, spin.spin_time);
        record.is_golden = spin.is_golden;
        WheelSpinRepository::new(&self.path)
            .log_wheel_spin(&record)
            .map_err(Into::into)
    }

    pub fn get_wheel_spin(
        &self,
        spin_id: i64,
    ) -> Result<Option<WheelSpinRecord>, GoldenWheelRepositoryError> {
        WheelSpinRepository::new(&self.path)
            .get_wheel_spin(spin_id)
            .map_err(Into::into)
    }

    /// Inspect the migrated Golden Wheel marker without mutating schema.
    pub fn is_golden_column_info(
        &self,
    ) -> Result<Option<GoldenWheelColumnInfo>, GoldenWheelRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT name, \"notnull\", dflt_value
                   FROM pragma_table_info('wheel_spins')
                  WHERE name = 'is_golden'",
                [],
                |row| {
                    Ok(GoldenWheelColumnInfo {
                        name: row.get(0)?,
                        not_null: row.get::<_, i64>(1)? != 0,
                        default_value: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

#[cfg(test)]
#[path = "golden_wheel_repository/tests.rs"]
mod tests;
