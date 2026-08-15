//! Python-compatible soft-avoid repository and service.
//!
//! Python remains the schema-migration authority during the side-by-side
//! rewrite. This module therefore opens only an existing database, writes the
//! existing `players`, `soft_avoids`, and economy-ledger tables, and never
//! creates or migrates caller data. Purchases use the same `BEGIN IMMEDIATE`
//! boundary as Python so two concurrent attempts for one pair can produce only
//! one debit and one activation.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

pub const SOFT_AVOID_GAMES: i64 = 10;
pub const SOFT_AVOID_MAX_GAMES: i64 = SOFT_AVOID_GAMES;

/// One row from Python's `soft_avoids` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoftAvoid {
    pub id: i64,
    pub guild_id: i64,
    pub avoider_discord_id: i64,
    pub avoided_discord_id: i64,
    pub games_remaining: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A non-exceptional reason that an atomic purchase did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftAvoidPurchaseFailureReason {
    AlreadyActive,
    InsufficientBalance,
}

/// Outcome of Python's atomic soft-avoid purchase operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoftAvoidPurchaseResult {
    pub success: bool,
    pub reason: Option<SoftAvoidPurchaseFailureReason>,
    pub balance: i64,
    pub avoid: Option<SoftAvoid>,
}

#[derive(Debug, Error)]
pub enum SoftAvoidRepositoryError {
    #[error("Cannot create soft avoid: avoider and avoided cannot be the same player")]
    SelfAvoid,
    #[error("Soft avoid duration must be between 1 and {SOFT_AVOID_MAX_GAMES} games")]
    InvalidDuration,
    #[error("Soft avoid cost cannot be negative")]
    NegativeCost,
    #[error("Soft avoid is already active with {games_remaining} games remaining")]
    AlreadyActive { games_remaining: i64 },
    #[error(
        "Registered player row not found for soft avoid buyer {avoider_id} in guild {guild_id}"
    )]
    MissingBuyer { avoider_id: i64, guild_id: i64 },
    #[error("Soft avoid debit failed after balance validation")]
    DebitFailed,
    #[error("Soft avoid write succeeded but row was not found")]
    MissingAvoidAfterWrite,
    #[error("Soft avoid purchase succeeded but activation row was not found")]
    MissingAvoidAfterPurchase,
    #[error("soft-avoid SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Operations delegated by the soft-avoid service.
pub trait SoftAvoidStore {
    type Error;

    fn create_or_reactivate_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        games: i64,
    ) -> Result<SoftAvoid, Self::Error>;

    fn purchase_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        cost: i64,
        games: i64,
    ) -> Result<SoftAvoidPurchaseResult, Self::Error>;

    fn get_active_avoids_for_players(
        &self,
        guild_id: Option<i64>,
        player_ids: &[i64],
    ) -> Result<Vec<SoftAvoid>, Self::Error>;

    fn get_user_avoids(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<Vec<SoftAvoid>, Self::Error>;

    fn decrement_avoids(
        &self,
        guild_id: Option<i64>,
        avoid_ids: &[i64],
    ) -> Result<usize, Self::Error>;

    fn delete_expired_avoids(&self, guild_id: Option<i64>) -> Result<usize, Self::Error>;
}

/// Repository for Python's existing soft-avoid and economy tables.
#[derive(Clone, Debug)]
pub struct SoftAvoidRepository {
    path: PathBuf,
}

impl SoftAvoidRepository {
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

    /// Create a new avoid or reactivate an expired row without charging.
    ///
    /// An active row is rejected rather than extended, matching Python's
    /// anti-stacking rule.
    pub fn create_or_reactivate_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        games: i64,
    ) -> Result<SoftAvoid, SoftAvoidRepositoryError> {
        validate_pair_and_games(avoider_id, avoided_id, games)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let now = unix_timestamp();

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = find_avoid(&transaction, guild_id, avoider_id, avoided_id)?;

        if let Some(existing) = &existing
            && existing.games_remaining > 0
        {
            return Err(SoftAvoidRepositoryError::AlreadyActive {
                games_remaining: existing.games_remaining,
            });
        }

        if let Some(existing) = existing {
            transaction.execute(
                "UPDATE soft_avoids
                 SET games_remaining = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![games, now, existing.id],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO soft_avoids (
                     guild_id, avoider_discord_id, avoided_discord_id,
                     games_remaining, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![guild_id, avoider_id, avoided_id, games, now],
            )?;
        }

        let avoid = find_avoid(&transaction, guild_id, avoider_id, avoided_id)?
            .ok_or(SoftAvoidRepositoryError::MissingAvoidAfterWrite)?;
        transaction.commit()?;
        Ok(avoid)
    }

    /// Atomically debit a registered player and activate one avoid.
    pub fn purchase_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        cost: i64,
        games: i64,
    ) -> Result<SoftAvoidPurchaseResult, SoftAvoidRepositoryError> {
        validate_pair_and_games(avoider_id, avoided_id, games)?;
        if cost < 0 {
            return Err(SoftAvoidRepositoryError::NegativeCost);
        }

        let guild_id = Self::normalize_guild_id(guild_id);
        let now = unix_timestamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0)
                 FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![avoider_id, guild_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(SoftAvoidRepositoryError::MissingBuyer {
                avoider_id,
                guild_id,
            })?;

        let existing = find_avoid(&transaction, guild_id, avoider_id, avoided_id)?;
        if existing
            .as_ref()
            .is_some_and(|avoid| avoid.games_remaining > 0)
        {
            let result = SoftAvoidPurchaseResult {
                success: false,
                reason: Some(SoftAvoidPurchaseFailureReason::AlreadyActive),
                balance,
                avoid: existing,
            };
            transaction.commit()?;
            return Ok(result);
        }
        if balance < cost {
            let result = SoftAvoidPurchaseResult {
                success: false,
                reason: Some(SoftAvoidPurchaseFailureReason::InsufficientBalance),
                balance,
                avoid: None,
            };
            transaction.commit()?;
            return Ok(result);
        }

        set_ledger_context(&transaction, avoider_id, avoided_id, cost, games)?;
        let debit_result = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) >= ?1",
            params![cost, avoider_id, guild_id],
        );
        // Python clears the one-row trigger context in a `finally` block.
        let clear_result = clear_ledger_context(&transaction);
        let changed = match debit_result {
            Ok(changed) => {
                clear_result?;
                changed
            }
            Err(error) => {
                let _ = clear_result;
                return Err(error.into());
            }
        };
        if changed == 0 {
            return Err(SoftAvoidRepositoryError::DebitFailed);
        }

        transaction.execute(
            "UPDATE players
             SET lowest_balance_ever = jopacoin_balance
             WHERE discord_id = ?1 AND guild_id = ?2
               AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)",
            params![avoider_id, guild_id],
        )?;

        if let Some(existing) = existing {
            transaction.execute(
                "UPDATE soft_avoids
                 SET games_remaining = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![games, now, existing.id],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO soft_avoids (
                     guild_id, avoider_discord_id, avoided_discord_id,
                     games_remaining, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![guild_id, avoider_id, avoided_id, games, now],
            )?;
        }

        let avoid = find_avoid(&transaction, guild_id, avoider_id, avoided_id)?
            .ok_or(SoftAvoidRepositoryError::MissingAvoidAfterPurchase)?;
        let result = SoftAvoidPurchaseResult {
            success: true,
            reason: None,
            balance: balance - cost,
            avoid: Some(avoid),
        };
        transaction.commit()?;
        Ok(result)
    }

    /// Return active avoids for which both endpoints are in `player_ids`.
    pub fn get_active_avoids_for_players(
        &self,
        guild_id: Option<i64>,
        player_ids: &[i64],
    ) -> Result<Vec<SoftAvoid>, SoftAvoidRepositoryError> {
        if player_ids.is_empty() {
            return Ok(Vec::new());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let placeholders = std::iter::repeat_n("?", player_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                    games_remaining, created_at, updated_at
             FROM soft_avoids
             WHERE guild_id = ?
               AND games_remaining > 0
               AND avoider_discord_id IN ({placeholders})
               AND avoided_discord_id IN ({placeholders})"
        );
        let parameters = std::iter::once(guild_id)
            .chain(player_ids.iter().copied())
            .chain(player_ids.iter().copied());
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(parameters), row_to_avoid)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Return active avoids created by one player, newest timestamp first.
    pub fn get_user_avoids(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<Vec<SoftAvoid>, SoftAvoidRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                    games_remaining, created_at, updated_at
             FROM soft_avoids
             WHERE guild_id = ?1 AND avoider_discord_id = ?2 AND games_remaining > 0
             ORDER BY created_at DESC",
        )?;
        let rows = statement
            .query_map(params![guild_id, discord_id], row_to_avoid)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Decrement active rows, stopping at zero.
    pub fn decrement_avoids(
        &self,
        guild_id: Option<i64>,
        avoid_ids: &[i64],
    ) -> Result<usize, SoftAvoidRepositoryError> {
        if avoid_ids.is_empty() {
            return Ok(0);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let now = unix_timestamp();
        let placeholders = std::iter::repeat_n("?", avoid_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE soft_avoids
             SET games_remaining = games_remaining - 1, updated_at = ?
             WHERE guild_id = ? AND id IN ({placeholders}) AND games_remaining > 0"
        );
        let parameters = std::iter::once(now)
            .chain(std::iter::once(guild_id))
            .chain(avoid_ids.iter().copied());
        Ok(self
            .connection()?
            .execute(&sql, params_from_iter(parameters))?)
    }

    /// Delete expired rows in one guild.
    pub fn delete_expired_avoids(
        &self,
        guild_id: Option<i64>,
    ) -> Result<usize, SoftAvoidRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        Ok(self.connection()?.execute(
            "DELETE FROM soft_avoids WHERE guild_id = ?1 AND games_remaining <= 0",
            [guild_id],
        )?)
    }
}

impl SoftAvoidStore for SoftAvoidRepository {
    type Error = SoftAvoidRepositoryError;

    fn create_or_reactivate_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        games: i64,
    ) -> Result<SoftAvoid, Self::Error> {
        Self::create_or_reactivate_avoid(self, guild_id, avoider_id, avoided_id, games)
    }

    fn purchase_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        cost: i64,
        games: i64,
    ) -> Result<SoftAvoidPurchaseResult, Self::Error> {
        Self::purchase_avoid(self, guild_id, avoider_id, avoided_id, cost, games)
    }

    fn get_active_avoids_for_players(
        &self,
        guild_id: Option<i64>,
        player_ids: &[i64],
    ) -> Result<Vec<SoftAvoid>, Self::Error> {
        Self::get_active_avoids_for_players(self, guild_id, player_ids)
    }

    fn get_user_avoids(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<Vec<SoftAvoid>, Self::Error> {
        Self::get_user_avoids(self, guild_id, discord_id)
    }

    fn decrement_avoids(
        &self,
        guild_id: Option<i64>,
        avoid_ids: &[i64],
    ) -> Result<usize, Self::Error> {
        Self::decrement_avoids(self, guild_id, avoid_ids)
    }

    fn delete_expired_avoids(&self, guild_id: Option<i64>) -> Result<usize, Self::Error> {
        Self::delete_expired_avoids(self, guild_id)
    }
}

/// Thin service wrapper matching Python's delegation boundary.
#[derive(Clone, Debug)]
pub struct SoftAvoidService<S> {
    repository: S,
}

impl<S> SoftAvoidService<S> {
    #[must_use]
    pub const fn new(repository: S) -> Self {
        Self { repository }
    }

    #[must_use]
    pub const fn repository(&self) -> &S {
        &self.repository
    }
}

impl<S: SoftAvoidStore> SoftAvoidService<S> {
    pub fn create_or_reactivate_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        games: i64,
    ) -> Result<SoftAvoid, S::Error> {
        self.repository
            .create_or_reactivate_avoid(guild_id, avoider_id, avoided_id, games)
    }

    pub fn purchase_avoid(
        &self,
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        cost: i64,
        games: i64,
    ) -> Result<SoftAvoidPurchaseResult, S::Error> {
        self.repository
            .purchase_avoid(guild_id, avoider_id, avoided_id, cost, games)
    }

    pub fn get_active_avoids_for_players(
        &self,
        guild_id: Option<i64>,
        player_ids: &[i64],
    ) -> Result<Vec<SoftAvoid>, S::Error> {
        self.repository
            .get_active_avoids_for_players(guild_id, player_ids)
    }

    pub fn get_user_avoids(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<Vec<SoftAvoid>, S::Error> {
        self.repository.get_user_avoids(guild_id, discord_id)
    }

    pub fn decrement_avoids(
        &self,
        guild_id: Option<i64>,
        avoid_ids: &[i64],
    ) -> Result<usize, S::Error> {
        self.repository.decrement_avoids(guild_id, avoid_ids)
    }

    pub fn delete_expired_avoids(&self, guild_id: Option<i64>) -> Result<usize, S::Error> {
        self.repository.delete_expired_avoids(guild_id)
    }
}

fn validate_pair_and_games(
    avoider_id: i64,
    avoided_id: i64,
    games: i64,
) -> Result<(), SoftAvoidRepositoryError> {
    if avoider_id == avoided_id {
        return Err(SoftAvoidRepositoryError::SelfAvoid);
    }
    if !(1..=SOFT_AVOID_MAX_GAMES).contains(&games) {
        return Err(SoftAvoidRepositoryError::InvalidDuration);
    }
    Ok(())
}

fn unix_timestamp() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

fn find_avoid(
    transaction: &Transaction<'_>,
    guild_id: i64,
    avoider_id: i64,
    avoided_id: i64,
) -> Result<Option<SoftAvoid>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                    games_remaining, created_at, updated_at
             FROM soft_avoids
             WHERE guild_id = ?1 AND avoider_discord_id = ?2 AND avoided_discord_id = ?3",
            params![guild_id, avoider_id, avoided_id],
            row_to_avoid,
        )
        .optional()
}

fn row_to_avoid(row: &Row<'_>) -> Result<SoftAvoid, rusqlite::Error> {
    Ok(SoftAvoid {
        id: row.get(0)?,
        guild_id: row.get(1)?,
        avoider_discord_id: row.get(2)?,
        avoided_discord_id: row.get(3)?,
        games_remaining: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    avoider_id: i64,
    avoided_id: i64,
    cost: i64,
    games: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'soft_avoid', ?1, 'player', ?2, 'soft avoid purchase', ?3)",
        params![
            avoider_id,
            avoided_id.to_string(),
            format!(r#"{{"cost": {cost}, "games": {games}}}"#),
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::{
        SoftAvoid, SoftAvoidPurchaseFailureReason, SoftAvoidPurchaseResult, SoftAvoidRepository,
        SoftAvoidRepositoryError, SoftAvoidService, SoftAvoidStore,
    };

    const TEST_GUILD_ID: i64 = 123;
    const SECOND_GUILD_ID: i64 = 456;
    const BUYER_ID: i64 = 100;
    const AVOIDED_ID: i64 = 200;

    struct Fixture {
        file: NamedTempFile,
        repository: SoftAvoidRepository,
    }

    impl Fixture {
        fn new() -> Self {
            let file = NamedTempFile::new().expect("create temporary SQLite file");
            let connection = Connection::open(file.path()).expect("open temporary SQLite file");
            connection
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE players (
                         discord_id          INTEGER NOT NULL,
                         guild_id            INTEGER NOT NULL DEFAULT 0,
                         discord_username    TEXT NOT NULL,
                         jopacoin_balance     INTEGER DEFAULT 3,
                         lowest_balance_ever INTEGER,
                         updated_at          TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                         PRIMARY KEY (discord_id, guild_id)
                     );
                     CREATE TABLE soft_avoids (
                         id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id            INTEGER NOT NULL DEFAULT 0,
                         avoider_discord_id  INTEGER NOT NULL,
                         avoided_discord_id  INTEGER NOT NULL,
                         games_remaining     INTEGER NOT NULL DEFAULT 10,
                         created_at          INTEGER NOT NULL,
                         updated_at          INTEGER NOT NULL,
                         UNIQUE (guild_id, avoider_discord_id, avoided_discord_id)
                     );
                     CREATE INDEX idx_soft_avoids_avoider
                         ON soft_avoids(guild_id, avoider_discord_id);
                     CREATE INDEX idx_soft_avoids_avoided
                         ON soft_avoids(guild_id, avoided_discord_id);
                     CREATE INDEX idx_soft_avoids_expired
                         ON soft_avoids(guild_id, games_remaining)
                         WHERE games_remaining <= 0;
                     CREATE TABLE economy_ledger_entries (
                         ledger_id      INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id       INTEGER NOT NULL,
                         account_type   TEXT NOT NULL
                             CHECK(account_type IN ('player', 'nonprofit')),
                         account_id     INTEGER,
                         delta          INTEGER NOT NULL,
                         balance_before INTEGER NOT NULL,
                         balance_after  INTEGER NOT NULL,
                         source         TEXT NOT NULL DEFAULT 'balance_update',
                         actor_id       INTEGER,
                         related_type   TEXT,
                         related_id     TEXT,
                         reason         TEXT,
                         metadata       TEXT,
                         created_at     INTEGER NOT NULL
                             DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
                     );
                     CREATE TABLE economy_ledger_context (
                         id           INTEGER PRIMARY KEY CHECK(id = 1),
                         source       TEXT,
                         actor_id     INTEGER,
                         related_type TEXT,
                         related_id   TEXT,
                         reason       TEXT,
                         metadata     TEXT
                     );
                     CREATE TRIGGER trg_economy_ledger_players_insert
                     AFTER INSERT ON players
                     WHEN COALESCE(NEW.jopacoin_balance, 0) != 0
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                             COALESCE(NEW.jopacoin_balance, 0), 0,
                             COALESCE(NEW.jopacoin_balance, 0),
                             COALESCE(
                                 (SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'player_insert'
                             ),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;
                     CREATE TRIGGER trg_economy_ledger_players_update
                     AFTER UPDATE OF jopacoin_balance ON players
                     WHEN COALESCE(OLD.jopacoin_balance, 0)
                          != COALESCE(NEW.jopacoin_balance, 0)
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                             COALESCE(NEW.jopacoin_balance, 0)
                                 - COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(NEW.jopacoin_balance, 0),
                             COALESCE(
                                 (SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'balance_update'
                             ),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;",
                )
                .expect("create Python-compatible soft-avoid schema");
            drop(connection);
            let repository = SoftAvoidRepository::new(file.path());
            Self { file, repository }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.file.path()).expect("open fixture database")
        }

        fn seed_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
            let connection = self.connection();
            connection
                .execute(
                    "INSERT INTO players (
                         discord_id, guild_id, discord_username, jopacoin_balance
                     ) VALUES (?1, ?2, 'Buyer', 3)",
                    params![discord_id, guild_id],
                )
                .expect("insert player like Python PlayerRepository.add");
            connection
                .execute(
                    "UPDATE players
                     SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?2 AND guild_id = ?3",
                    params![balance, discord_id, guild_id],
                )
                .expect("set player balance");
            connection
                .execute(
                    "UPDATE players SET lowest_balance_ever = ?1
                     WHERE discord_id = ?2 AND guild_id = ?3
                       AND (lowest_balance_ever IS NULL OR ?1 < lowest_balance_ever)",
                    params![balance, discord_id, guild_id],
                )
                .expect("initialize lowest balance");
        }

        fn balance_and_lowest(&self, discord_id: i64, guild_id: i64) -> (i64, Option<i64>) {
            self.connection()
                .query_row(
                    "SELECT COALESCE(jopacoin_balance, 0), lowest_balance_ever
                     FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, guild_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("read player balances")
        }

        fn soft_ledger_count(&self, discord_id: i64, guild_id: i64) -> i64 {
            self.connection()
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id = ?1 AND account_id = ?2 AND source = 'soft_avoid'",
                    params![guild_id, discord_id],
                    |row| row.get(0),
                )
                .expect("count soft-avoid ledger entries")
        }
    }

    #[test]
    fn test_create_avoid() {
        let fixture = Fixture::new();
        let avoid = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 10)
            .unwrap();
        assert_eq!(avoid.avoider_discord_id, BUYER_ID);
        assert_eq!(avoid.avoided_discord_id, AVOIDED_ID);
        assert_eq!(avoid.games_remaining, 10);
        assert_eq!(avoid.guild_id, TEST_GUILD_ID);
    }

    #[test]
    fn test_active_avoid_cannot_be_extended() {
        let fixture = Fixture::new();
        let first = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 10)
            .unwrap();
        assert_eq!(first.games_remaining, 10);
        let error = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 10)
            .expect_err("active avoids cannot be extended");
        assert!(matches!(
            error,
            SoftAvoidRepositoryError::AlreadyActive {
                games_remaining: 10
            }
        ));
        assert_eq!(
            fixture
                .repository
                .get_user_avoids(Some(TEST_GUILD_ID), BUYER_ID)
                .unwrap()[0]
                .games_remaining,
            10
        );
    }

    #[test]
    fn test_expired_avoid_can_be_reactivated() {
        let fixture = Fixture::new();
        let first = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 1)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .decrement_avoids(Some(TEST_GUILD_ID), &[first.id])
                .unwrap(),
            1
        );
        let second = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 10)
            .unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.games_remaining, 10);
    }

    fn assert_invalid_create_duration(games: i64) {
        let fixture = Fixture::new();
        let error = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, games)
            .expect_err("invalid duration must fail");
        assert!(matches!(error, SoftAvoidRepositoryError::InvalidDuration));
        assert!(error.to_string().contains("between 1 and 10 games"));
    }

    #[test]
    fn test_duration_must_be_between_one_and_ten_games_negative_1() {
        assert_invalid_create_duration(-1);
    }

    #[test]
    fn test_duration_must_be_between_one_and_ten_games_0() {
        assert_invalid_create_duration(0);
    }

    #[test]
    fn test_duration_must_be_between_one_and_ten_games_11() {
        assert_invalid_create_duration(11);
    }

    fn assert_invalid_purchase_duration(games: i64) {
        let fixture = Fixture::new();
        let error = fixture
            .repository
            .purchase_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 250, games)
            .expect_err("invalid purchase duration must fail");
        assert!(matches!(error, SoftAvoidRepositoryError::InvalidDuration));
        assert!(error.to_string().contains("between 1 and 10 games"));
    }

    #[test]
    fn test_purchase_duration_must_be_between_one_and_ten_games_negative_1() {
        assert_invalid_purchase_duration(-1);
    }

    #[test]
    fn test_purchase_duration_must_be_between_one_and_ten_games_0() {
        assert_invalid_purchase_duration(0);
    }

    #[test]
    fn test_purchase_duration_must_be_between_one_and_ten_games_11() {
        assert_invalid_purchase_duration(11);
    }

    #[test]
    fn test_purchase_avoid_debits_once_and_writes_one_ledger_entry() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, TEST_GUILD_ID, 1_000);

        let result = fixture
            .repository
            .purchase_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 250, 10)
            .unwrap();

        assert!(result.success);
        assert_eq!(result.reason, None);
        assert_eq!(result.balance, 750);
        assert_eq!(result.avoid.expect("activated avoid").games_remaining, 10);
        assert_eq!(
            fixture.balance_and_lowest(BUYER_ID, TEST_GUILD_ID),
            (750, Some(750))
        );

        let connection = fixture.connection();
        let ledger_rows = connection
            .prepare(
                "SELECT delta, source, related_type, related_id, actor_id, reason, metadata
                 FROM economy_ledger_entries
                 WHERE guild_id = ?1 AND account_id = ?2 AND source = 'soft_avoid'
                 ORDER BY ledger_id",
            )
            .unwrap()
            .query_map(params![TEST_GUILD_ID, BUYER_ID], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            ledger_rows,
            [(
                -250,
                "soft_avoid".to_owned(),
                Some("player".to_owned()),
                Some(AVOIDED_ID.to_string()),
                Some(BUYER_ID),
                Some("soft avoid purchase".to_owned()),
                Some(r#"{"cost": 250, "games": 10}"#.to_owned()),
            )]
        );
    }

    #[test]
    fn test_activation_failure_rolls_back_debit_and_ledger() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, TEST_GUILD_ID, 1_000);
        fixture
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_soft_avoid_activation
                 BEFORE INSERT ON soft_avoids
                 BEGIN
                     SELECT RAISE(ABORT, 'forced activation failure');
                 END;",
            )
            .unwrap();

        let error = fixture
            .repository
            .purchase_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 250, 10)
            .expect_err("activation trigger must abort the purchase");
        assert!(matches!(error, SoftAvoidRepositoryError::Sqlite(_)));
        assert!(error.to_string().contains("forced activation failure"));
        assert_eq!(
            fixture.balance_and_lowest(BUYER_ID, TEST_GUILD_ID),
            (1_000, Some(1_000))
        );
        assert!(
            fixture
                .repository
                .get_user_avoids(Some(TEST_GUILD_ID), BUYER_ID)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fixture.soft_ledger_count(BUYER_ID, TEST_GUILD_ID), 0);
    }

    #[test]
    fn test_concurrent_duplicate_purchase_has_one_charge_and_one_activation() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, TEST_GUILD_ID, 1_000);
        let repository = Arc::new(fixture.repository.clone());
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                repository
                    .purchase_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 250, 10)
                    .unwrap()
            }));
        }
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("purchase thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.success).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.reason == Some(SoftAvoidPurchaseFailureReason::AlreadyActive)
                })
                .count(),
            1
        );
        assert_eq!(
            fixture.balance_and_lowest(BUYER_ID, TEST_GUILD_ID),
            (750, Some(750))
        );
        let active = repository
            .get_user_avoids(Some(TEST_GUILD_ID), BUYER_ID)
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].games_remaining, 10);
        assert_eq!(fixture.soft_ledger_count(BUYER_ID, TEST_GUILD_ID), 1);
    }

    #[test]
    fn test_insufficient_balance_does_not_activate_or_touch_lowest_balance() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, TEST_GUILD_ID, 249);
        let result = fixture
            .repository
            .purchase_avoid(Some(TEST_GUILD_ID), BUYER_ID, AVOIDED_ID, 250, 10)
            .unwrap();
        assert!(!result.success);
        assert_eq!(
            result.reason,
            Some(SoftAvoidPurchaseFailureReason::InsufficientBalance)
        );
        assert_eq!(result.balance, 249);
        assert_eq!(result.avoid, None);
        assert_eq!(
            fixture.balance_and_lowest(BUYER_ID, TEST_GUILD_ID),
            (249, Some(249))
        );
        assert!(
            fixture
                .repository
                .get_user_avoids(Some(TEST_GUILD_ID), BUYER_ID)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fixture.soft_ledger_count(BUYER_ID, TEST_GUILD_ID), 0);
    }

    #[test]
    fn test_get_active_avoids_for_players() {
        let fixture = Fixture::new();
        for (avoider, avoided, games) in [(100, 200, 10), (200, 300, 5), (400, 500, 10)] {
            fixture
                .repository
                .create_or_reactivate_avoid(Some(TEST_GUILD_ID), avoider, avoided, games)
                .unwrap();
        }
        let avoids = fixture
            .repository
            .get_active_avoids_for_players(Some(TEST_GUILD_ID), &[100, 200, 300])
            .unwrap();
        assert_eq!(avoids.len(), 2);
        let avoider_ids = avoids
            .into_iter()
            .map(|avoid| avoid.avoider_discord_id)
            .collect::<std::collections::HashSet<_>>();
        assert!(avoider_ids.contains(&100));
        assert!(avoider_ids.contains(&200));
    }

    #[test]
    fn test_get_active_avoids_excludes_zero_remaining() {
        let fixture = Fixture::new();
        let avoid = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 1)
            .unwrap();
        fixture
            .repository
            .decrement_avoids(Some(TEST_GUILD_ID), &[avoid.id])
            .unwrap();
        assert!(
            fixture
                .repository
                .get_active_avoids_for_players(Some(TEST_GUILD_ID), &[100, 200])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_get_user_avoids() {
        let fixture = Fixture::new();
        for (avoider, avoided, games) in [(100, 200, 10), (100, 300, 5), (400, 100, 10)] {
            fixture
                .repository
                .create_or_reactivate_avoid(Some(TEST_GUILD_ID), avoider, avoided, games)
                .unwrap();
        }
        let avoids = fixture
            .repository
            .get_user_avoids(Some(TEST_GUILD_ID), 100)
            .unwrap();
        assert_eq!(avoids.len(), 2);
        assert!(avoids.iter().all(|avoid| avoid.avoider_discord_id == 100));
    }

    #[test]
    fn test_decrement_avoids() {
        let fixture = Fixture::new();
        let first = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 10)
            .unwrap();
        let second = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 300, 400, 5)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .decrement_avoids(Some(TEST_GUILD_ID), &[first.id, second.id])
                .unwrap(),
            2
        );
        let remaining = fixture
            .repository
            .get_active_avoids_for_players(Some(TEST_GUILD_ID), &[100, 200, 300, 400])
            .unwrap()
            .into_iter()
            .map(|avoid| (avoid.id, avoid.games_remaining))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(remaining[&first.id], 9);
        assert_eq!(remaining[&second.id], 4);
    }

    #[test]
    fn test_decrement_stops_at_zero() {
        let fixture = Fixture::new();
        let avoid = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 1)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .decrement_avoids(Some(TEST_GUILD_ID), &[avoid.id])
                .unwrap(),
            1
        );
        assert_eq!(
            fixture
                .repository
                .decrement_avoids(Some(TEST_GUILD_ID), &[avoid.id])
                .unwrap(),
            0
        );
    }

    #[test]
    fn test_delete_expired_avoids() {
        let fixture = Fixture::new();
        let expired = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 1)
            .unwrap();
        fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 300, 400, 10)
            .unwrap();
        fixture
            .repository
            .decrement_avoids(Some(TEST_GUILD_ID), &[expired.id])
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .delete_expired_avoids(Some(TEST_GUILD_ID))
                .unwrap(),
            1
        );
        let avoids = fixture
            .repository
            .get_active_avoids_for_players(Some(TEST_GUILD_ID), &[100, 200, 300, 400])
            .unwrap();
        assert_eq!(avoids.len(), 1);
        assert_eq!(avoids[0].avoider_discord_id, 300);
    }

    #[test]
    fn test_guild_isolation() {
        let fixture = Fixture::new();
        fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 10)
            .unwrap();
        fixture
            .repository
            .create_or_reactivate_avoid(Some(SECOND_GUILD_ID), 100, 200, 5)
            .unwrap();
        let first = fixture
            .repository
            .get_active_avoids_for_players(Some(TEST_GUILD_ID), &[100, 200])
            .unwrap();
        let second = fixture
            .repository
            .get_active_avoids_for_players(Some(SECOND_GUILD_ID), &[100, 200])
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].games_remaining, 10);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].games_remaining, 5);
    }

    #[test]
    fn test_empty_player_list_returns_empty() {
        let fixture = Fixture::new();
        fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 10)
            .unwrap();
        assert!(
            fixture
                .repository
                .get_active_avoids_for_players(Some(TEST_GUILD_ID), &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_no_matching_avoids_returns_empty() {
        let fixture = Fixture::new();
        fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 10)
            .unwrap();
        assert!(
            fixture
                .repository
                .get_active_avoids_for_players(Some(TEST_GUILD_ID), &[300, 400])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_self_avoid_raises_error() {
        let fixture = Fixture::new();
        let error = fixture
            .repository
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 100, 10)
            .expect_err("self avoid must fail");
        assert!(matches!(error, SoftAvoidRepositoryError::SelfAvoid));
        assert!(error.to_string().to_lowercase().contains("same player"));
    }

    #[test]
    fn test_create_or_reactivate_avoid_delegates() {
        let fixture = Fixture::new();
        let service = SoftAvoidService::new(fixture.repository.clone());
        let avoid = service
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 200, 3)
            .unwrap();
        assert_eq!(avoid.avoider_discord_id, 100);
        assert_eq!(avoid.avoided_discord_id, 200);
        assert_eq!(avoid.games_remaining, 3);
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct PurchaseCall {
        guild_id: Option<i64>,
        avoider_id: i64,
        avoided_id: i64,
        cost: i64,
        games: i64,
    }

    #[derive(Debug)]
    struct RecordingStore {
        result: SoftAvoidPurchaseResult,
        calls: Mutex<Vec<PurchaseCall>>,
    }

    impl SoftAvoidStore for RecordingStore {
        type Error = Infallible;

        fn create_or_reactivate_avoid(
            &self,
            _guild_id: Option<i64>,
            _avoider_id: i64,
            _avoided_id: i64,
            _games: i64,
        ) -> Result<SoftAvoid, Self::Error> {
            unreachable!("not used by this delegation test")
        }

        fn purchase_avoid(
            &self,
            guild_id: Option<i64>,
            avoider_id: i64,
            avoided_id: i64,
            cost: i64,
            games: i64,
        ) -> Result<SoftAvoidPurchaseResult, Self::Error> {
            self.calls.lock().unwrap().push(PurchaseCall {
                guild_id,
                avoider_id,
                avoided_id,
                cost,
                games,
            });
            Ok(self.result.clone())
        }

        fn get_active_avoids_for_players(
            &self,
            _guild_id: Option<i64>,
            _player_ids: &[i64],
        ) -> Result<Vec<SoftAvoid>, Self::Error> {
            unreachable!("not used by this delegation test")
        }

        fn get_user_avoids(
            &self,
            _guild_id: Option<i64>,
            _discord_id: i64,
        ) -> Result<Vec<SoftAvoid>, Self::Error> {
            unreachable!("not used by this delegation test")
        }

        fn decrement_avoids(
            &self,
            _guild_id: Option<i64>,
            _avoid_ids: &[i64],
        ) -> Result<usize, Self::Error> {
            unreachable!("not used by this delegation test")
        }

        fn delete_expired_avoids(&self, _guild_id: Option<i64>) -> Result<usize, Self::Error> {
            unreachable!("not used by this delegation test")
        }
    }

    #[test]
    fn test_purchase_avoid_delegates() {
        let expected = SoftAvoidPurchaseResult {
            success: true,
            reason: None,
            balance: 750,
            avoid: None,
        };
        let service = SoftAvoidService::new(RecordingStore {
            result: expected.clone(),
            calls: Mutex::new(Vec::new()),
        });
        let result = service
            .purchase_avoid(Some(TEST_GUILD_ID), 100, 200, 250, 10)
            .unwrap();
        assert_eq!(result, expected);
        assert_eq!(
            *service.repository().calls.lock().unwrap(),
            [PurchaseCall {
                guild_id: Some(TEST_GUILD_ID),
                avoider_id: 100,
                avoided_id: 200,
                cost: 250,
                games: 10,
            }]
        );
    }

    #[test]
    fn test_create_or_reactivate_rejects_self_avoid() {
        let fixture = Fixture::new();
        let service = SoftAvoidService::new(fixture.repository.clone());
        let error = service
            .create_or_reactivate_avoid(Some(TEST_GUILD_ID), 100, 100, 1)
            .expect_err("self avoid must fail through service");
        assert!(matches!(error, SoftAvoidRepositoryError::SelfAvoid));
        assert!(error.to_string().contains("same player"));
    }

    #[test]
    fn test_get_active_avoids_for_players_none_guild_matches_zero() {
        let fixture = Fixture::new();
        let service = SoftAvoidService::new(fixture.repository.clone());
        service.create_or_reactivate_avoid(None, 10, 20, 2).unwrap();
        let via_none = service
            .get_active_avoids_for_players(None, &[10, 20])
            .unwrap();
        let via_zero = service
            .get_active_avoids_for_players(Some(0), &[10, 20])
            .unwrap();
        assert_eq!(via_none.len(), 1);
        assert_eq!(
            via_zero[0].avoider_discord_id,
            via_none[0].avoider_discord_id
        );
    }
}
