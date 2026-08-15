//! Python-compatible package-deal persistence and scalar pricing.
//!
//! Python remains the sole schema-migration authority during the side-by-side
//! rewrite. This module opens only an existing SQLite file and uses the
//! already-migrated `players`, `package_deals`, `package_deal_purchases`, and
//! economy-ledger tables. Paid purchases use `BEGIN IMMEDIATE`, matching the
//! Python repository's serialization boundary for pricing, debit, and top-up.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

pub const PACKAGE_DEAL_MAX_GAMES: i64 = 10;
pub const PACKAGE_DEAL_NO_ACTIVE_COST: i64 = 1;
pub const SHOP_PACKAGE_DEAL_BASE_COST: i64 = 500;
pub const SHOP_PACKAGE_DEAL_RATING_DIVISOR: f64 = 10.0;
pub const DEFAULT_PACKAGE_DEAL_RATING: f64 = 1_500.0;

/// One mutable row from Python's `package_deals` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDeal {
    pub id: i64,
    pub guild_id: i64,
    pub buyer_discord_id: i64,
    pub partner_discord_id: i64,
    pub games_remaining: i64,
    pub cost_paid: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Number of games committed by the operation returning this value.
    pub games_added: i64,
}

/// One immutable row from Python's `package_deal_purchases` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDealPurchase {
    pub guild_id: i64,
    pub buyer_discord_id: i64,
    pub partner_discord_id: i64,
    pub jc_spent: i64,
    pub games_committed: i64,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageDealPurchaseFailureReason {
    AlreadyFull,
    InsufficientBalance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDealPurchaseResult {
    pub success: bool,
    pub reason: Option<PackageDealPurchaseFailureReason>,
    pub balance: i64,
    pub deal: Option<PackageDeal>,
    pub cost: i64,
    pub active_deal_count: i64,
}

#[derive(Debug, Error)]
pub enum PackageDealRepositoryError {
    #[error("Cannot create package deal: buyer and partner cannot be the same player")]
    SelfDeal,
    #[error("Package deal duration must be at least 1 game")]
    InvalidDuration,
    #[error("Package deal costs cannot be negative")]
    NegativeCost,
    #[error(
        "Registered player row not found for package deal buyer {buyer_id} in guild {guild_id}"
    )]
    MissingBuyer { buyer_id: i64, guild_id: i64 },
    #[error("Package deal debit failed after balance validation")]
    DebitFailed,
    #[error("package-deal write succeeded but the row was not found")]
    MissingDealAfterWrite,
    #[error("package-deal SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Calculate the command-layer scalar price using Python's defaults.
///
/// Python uses `rating or 1500`, so both a missing rating and numeric zero use
/// the default. Converting the rating bonus to `i64` truncates toward zero,
/// matching Python's `int(float)` for finite values.
#[must_use]
pub fn calculate_package_deal_cost(
    active_deal_count: i64,
    is_extend: bool,
    buyer_rating: Option<f64>,
    partner_rating: Option<f64>,
) -> i64 {
    if active_deal_count == 0 && !is_extend {
        return PACKAGE_DEAL_NO_ACTIVE_COST;
    }

    let buyer_rating = python_rating_or_default(buyer_rating);
    let partner_rating = python_rating_or_default(partner_rating);
    SHOP_PACKAGE_DEAL_BASE_COST
        + ((buyer_rating + partner_rating) / SHOP_PACKAGE_DEAL_RATING_DIVISOR) as i64
}

fn python_rating_or_default(rating: Option<f64>) -> f64 {
    match rating {
        Some(value) if value != 0.0 => value,
        _ => DEFAULT_PACKAGE_DEAL_RATING,
    }
}

/// Repository for Python's existing package-deal and economy tables.
#[derive(Clone, Debug)]
pub struct PackageDealRepository {
    path: PathBuf,
}

impl PackageDealRepository {
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

    /// Create a new deal or top an existing pair up to ten games without a debit.
    pub fn create_or_extend_deal(
        &self,
        guild_id: Option<i64>,
        buyer_id: i64,
        partner_id: i64,
        games: i64,
        cost: i64,
    ) -> Result<PackageDeal, PackageDealRepositoryError> {
        validate_pair_and_games(buyer_id, partner_id, games)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let now = unix_timestamp();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = find_deal(&transaction, guild_id, buyer_id, partner_id)?;
        let (games_added, games_remaining) = calculate_top_up(existing.as_ref(), games);

        if games_added == 0 {
            let mut deal = existing.ok_or(PackageDealRepositoryError::MissingDealAfterWrite)?;
            deal.games_added = 0;
            transaction.commit()?;
            return Ok(deal);
        }

        let deal = write_deal_top_up(
            &transaction,
            guild_id,
            buyer_id,
            partner_id,
            games_remaining,
            games_added,
            cost,
            now,
        )?;
        transaction.commit()?;
        Ok(deal)
    }

    /// Atomically choose introductory/active pricing, debit, and top up a deal.
    #[allow(clippy::too_many_arguments)]
    pub fn purchase_deal(
        &self,
        guild_id: Option<i64>,
        buyer_id: i64,
        partner_id: i64,
        games: i64,
        no_active_cost: i64,
        active_cost: i64,
    ) -> Result<PackageDealPurchaseResult, PackageDealRepositoryError> {
        validate_pair_and_games(buyer_id, partner_id, games)?;
        if no_active_cost < 0 || active_cost < 0 {
            return Err(PackageDealRepositoryError::NegativeCost);
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
                params![buyer_id, guild_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(PackageDealRepositoryError::MissingBuyer { buyer_id, guild_id })?;

        let active_deal_count = transaction.query_row(
            "SELECT COUNT(*)
             FROM package_deals
             WHERE guild_id = ?1 AND buyer_discord_id = ?2 AND games_remaining > 0",
            params![guild_id, buyer_id],
            |row| row.get(0),
        )?;
        let cost = if active_deal_count == 0 {
            no_active_cost
        } else {
            active_cost
        };

        let existing = find_deal(&transaction, guild_id, buyer_id, partner_id)?;
        let (games_added, games_remaining) = calculate_top_up(existing.as_ref(), games);
        if games_added == 0 {
            let mut deal = existing.ok_or(PackageDealRepositoryError::MissingDealAfterWrite)?;
            deal.games_added = 0;
            let result = PackageDealPurchaseResult {
                success: false,
                reason: Some(PackageDealPurchaseFailureReason::AlreadyFull),
                balance,
                deal: Some(deal),
                cost,
                active_deal_count,
            };
            transaction.commit()?;
            return Ok(result);
        }

        if balance < cost {
            let result = PackageDealPurchaseResult {
                success: false,
                reason: Some(PackageDealPurchaseFailureReason::InsufficientBalance),
                balance,
                deal: None,
                cost,
                active_deal_count,
            };
            transaction.commit()?;
            return Ok(result);
        }

        set_ledger_context(&transaction, buyer_id, partner_id, cost, games, games_added)?;
        let debit_result = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) >= ?1",
            params![cost, buyer_id, guild_id],
        );
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
            return Err(PackageDealRepositoryError::DebitFailed);
        }

        transaction.execute(
            "UPDATE players
             SET lowest_balance_ever = jopacoin_balance
             WHERE discord_id = ?1 AND guild_id = ?2
               AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)",
            params![buyer_id, guild_id],
        )?;

        let deal = write_deal_top_up(
            &transaction,
            guild_id,
            buyer_id,
            partner_id,
            games_remaining,
            games_added,
            cost,
            now,
        )?;
        let result = PackageDealPurchaseResult {
            success: true,
            reason: None,
            balance: balance - cost,
            deal: Some(deal),
            cost,
            active_deal_count,
        };
        transaction.commit()?;
        Ok(result)
    }

    /// Return active deals for which both endpoints occur in `player_ids`.
    pub fn get_active_deals_for_players(
        &self,
        guild_id: Option<i64>,
        player_ids: &[i64],
    ) -> Result<Vec<PackageDeal>, PackageDealRepositoryError> {
        if player_ids.is_empty() {
            return Ok(Vec::new());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let placeholders = std::iter::repeat_n("?", player_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                    games_remaining, cost_paid, created_at, updated_at
             FROM package_deals
             WHERE guild_id = ?
               AND games_remaining > 0
               AND buyer_discord_id IN ({placeholders})
               AND partner_discord_id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(1 + player_ids.len() * 2);
        values.push(guild_id);
        values.extend_from_slice(player_ids);
        values.extend_from_slice(player_ids);

        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let deals = statement
            .query_map(params_from_iter(values), row_to_deal)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(deals)
    }

    /// Return active deals where a player is either endpoint.
    pub fn get_deals_involving_player(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<Vec<PackageDeal>, PackageDealRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                    games_remaining, cost_paid, created_at, updated_at
             FROM package_deals
             WHERE guild_id = ?1
               AND (buyer_discord_id = ?2 OR partner_discord_id = ?2)
               AND games_remaining > 0
             ORDER BY created_at DESC",
        )?;
        let deals = statement
            .query_map(params![guild_id, discord_id], row_to_deal)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(deals)
    }

    /// Read immutable purchases in Python's inclusive-start/exclusive-end window.
    pub fn get_purchases_involving_player(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<PackageDealPurchase>, PackageDealRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT guild_id, buyer_discord_id, partner_discord_id,
                    jc_spent, games_committed, created_at
             FROM package_deal_purchases
             WHERE guild_id = ?1
               AND (buyer_discord_id = ?2 OR partner_discord_id = ?2)
               AND created_at >= ?3
               AND created_at < ?4
             ORDER BY created_at DESC",
        )?;
        let purchases = statement
            .query_map(
                params![guild_id, discord_id, start_ts, end_ts],
                row_to_purchase,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(purchases)
    }

    /// Return a buyer's active deals.
    pub fn get_user_deals(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<Vec<PackageDeal>, PackageDealRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                    games_remaining, cost_paid, created_at, updated_at
             FROM package_deals
             WHERE guild_id = ?1 AND buyer_discord_id = ?2 AND games_remaining > 0
             ORDER BY created_at DESC",
        )?;
        let deals = statement
            .query_map(params![guild_id, discord_id], row_to_deal)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(deals)
    }

    /// Consume one game from each still-active deal ID in the given guild.
    pub fn decrement_deals(
        &self,
        guild_id: Option<i64>,
        deal_ids: &[i64],
    ) -> Result<usize, PackageDealRepositoryError> {
        if deal_ids.is_empty() {
            return Ok(0);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let now = unix_timestamp();
        let placeholders = std::iter::repeat_n("?", deal_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE package_deals
             SET games_remaining = games_remaining - 1, updated_at = ?
             WHERE guild_id = ? AND id IN ({placeholders}) AND games_remaining > 0"
        );
        let mut values = Vec::with_capacity(2 + deal_ids.len());
        values.push(now);
        values.push(guild_id);
        values.extend_from_slice(deal_ids);
        let connection = self.connection()?;
        Ok(connection.execute(&sql, params_from_iter(values))?)
    }

    /// Delete consumed rows while leaving the immutable purchase log intact.
    pub fn delete_expired_deals(
        &self,
        guild_id: Option<i64>,
    ) -> Result<usize, PackageDealRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        Ok(connection.execute(
            "DELETE FROM package_deals WHERE guild_id = ?1 AND games_remaining <= 0",
            params![guild_id],
        )?)
    }
}

fn validate_pair_and_games(
    buyer_id: i64,
    partner_id: i64,
    games: i64,
) -> Result<(), PackageDealRepositoryError> {
    if buyer_id == partner_id {
        return Err(PackageDealRepositoryError::SelfDeal);
    }
    if games < 1 {
        return Err(PackageDealRepositoryError::InvalidDuration);
    }
    Ok(())
}

fn calculate_top_up(existing: Option<&PackageDeal>, games: i64) -> (i64, i64) {
    let current_games = existing.map_or(0, |deal| {
        deal.games_remaining.clamp(0, PACKAGE_DEAL_MAX_GAMES)
    });
    let games_added = games.min(PACKAGE_DEAL_MAX_GAMES - current_games);
    (games_added, current_games + games_added)
}

#[allow(clippy::too_many_arguments)]
fn write_deal_top_up(
    transaction: &Transaction<'_>,
    guild_id: i64,
    buyer_id: i64,
    partner_id: i64,
    games_remaining: i64,
    games_added: i64,
    cost: i64,
    now: i64,
) -> Result<PackageDeal, PackageDealRepositoryError> {
    transaction.execute(
        "INSERT INTO package_deals (
             guild_id, buyer_discord_id, partner_discord_id,
             games_remaining, cost_paid, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT(guild_id, buyer_discord_id, partner_discord_id)
         DO UPDATE SET
             games_remaining = excluded.games_remaining,
             cost_paid = cost_paid + excluded.cost_paid,
             updated_at = excluded.updated_at",
        params![guild_id, buyer_id, partner_id, games_remaining, cost, now],
    )?;
    transaction.execute(
        "INSERT INTO package_deal_purchases (
             guild_id, buyer_discord_id, partner_discord_id,
             jc_spent, games_committed, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![guild_id, buyer_id, partner_id, cost, games_added, now],
    )?;
    let mut deal = find_deal(transaction, guild_id, buyer_id, partner_id)?
        .ok_or(PackageDealRepositoryError::MissingDealAfterWrite)?;
    deal.games_added = games_added;
    Ok(deal)
}

fn find_deal(
    connection: &Connection,
    guild_id: i64,
    buyer_id: i64,
    partner_id: i64,
) -> Result<Option<PackageDeal>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                    games_remaining, cost_paid, created_at, updated_at
             FROM package_deals
             WHERE guild_id = ?1 AND buyer_discord_id = ?2 AND partner_discord_id = ?3",
            params![guild_id, buyer_id, partner_id],
            row_to_deal,
        )
        .optional()
}

fn row_to_deal(row: &Row<'_>) -> Result<PackageDeal, rusqlite::Error> {
    Ok(PackageDeal {
        id: row.get(0)?,
        guild_id: row.get(1)?,
        buyer_discord_id: row.get(2)?,
        partner_discord_id: row.get(3)?,
        games_remaining: row.get(4)?,
        cost_paid: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        games_added: 0,
    })
}

fn row_to_purchase(row: &Row<'_>) -> Result<PackageDealPurchase, rusqlite::Error> {
    Ok(PackageDealPurchase {
        guild_id: row.get(0)?,
        buyer_discord_id: row.get(1)?,
        partner_discord_id: row.get(2)?,
        jc_spent: row.get(3)?,
        games_committed: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    buyer_id: i64,
    partner_id: i64,
    cost: i64,
    games: i64,
    games_added: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (
             1, 'package_deal', ?1, 'player', ?2,
             'package deal purchase', ?3
         )",
        params![
            buyer_id,
            partner_id.to_string(),
            format!(r#"{{"cost": {cost}, "games": {games}, "games_added": {games_added}}}"#)
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn unix_timestamp() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::{
        PACKAGE_DEAL_NO_ACTIVE_COST, PackageDealPurchaseFailureReason, PackageDealRepository,
        PackageDealRepositoryError, SHOP_PACKAGE_DEAL_BASE_COST, SHOP_PACKAGE_DEAL_RATING_DIVISOR,
        calculate_package_deal_cost,
    };

    const GUILD_ID: i64 = 123;
    const BUYER_ID: i64 = 100;
    const PARTNER_ID: i64 = 200;

    struct Fixture {
        file: NamedTempFile,
        repository: PackageDealRepository,
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
                     CREATE TABLE package_deals (
                         id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id             INTEGER NOT NULL DEFAULT 0,
                         buyer_discord_id     INTEGER NOT NULL,
                         partner_discord_id   INTEGER NOT NULL,
                         games_remaining      INTEGER NOT NULL DEFAULT 10,
                         cost_paid             INTEGER NOT NULL,
                         created_at            INTEGER NOT NULL,
                         updated_at            INTEGER NOT NULL,
                         UNIQUE (guild_id, buyer_discord_id, partner_discord_id)
                     );
                     CREATE INDEX idx_package_deals_buyer
                         ON package_deals(guild_id, buyer_discord_id);
                     CREATE INDEX idx_package_deals_partner
                         ON package_deals(guild_id, partner_discord_id);
                     CREATE INDEX idx_package_deals_guild_active
                         ON package_deals(guild_id, games_remaining);
                     CREATE TRIGGER trg_package_deals_games_remaining_insert_cap
                     BEFORE INSERT ON package_deals
                     WHEN NEW.games_remaining > 10
                     BEGIN
                         SELECT RAISE(ABORT, 'package deal games_remaining cannot exceed 10');
                     END;
                     CREATE TRIGGER trg_package_deals_games_remaining_update_cap
                     BEFORE UPDATE OF games_remaining ON package_deals
                     WHEN NEW.games_remaining > 10
                     BEGIN
                         SELECT RAISE(ABORT, 'package deal games_remaining cannot exceed 10');
                     END;
                     CREATE TABLE package_deal_purchases (
                         id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id             INTEGER NOT NULL DEFAULT 0,
                         buyer_discord_id     INTEGER NOT NULL,
                         partner_discord_id   INTEGER NOT NULL,
                         jc_spent              INTEGER NOT NULL,
                         games_committed       INTEGER NOT NULL,
                         created_at            INTEGER NOT NULL
                     );
                     CREATE INDEX idx_package_deal_purchases_buyer
                         ON package_deal_purchases(guild_id, buyer_discord_id, created_at);
                     CREATE INDEX idx_package_deal_purchases_partner
                         ON package_deal_purchases(guild_id, partner_discord_id, created_at);
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
                .expect("create Python-compatible package-deal schema");
            drop(connection);
            let repository = PackageDealRepository::new(file.path());
            Self { file, repository }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.file.path()).expect("open fixture database")
        }

        fn seed_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
            self.connection()
                .execute(
                    "INSERT INTO players (
                         discord_id, guild_id, discord_username,
                         jopacoin_balance, lowest_balance_ever
                     ) VALUES (?1, ?2, 'Buyer', ?3, ?3)",
                    params![discord_id, guild_id, balance],
                )
                .expect("seed registered player");
        }

        fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
            self.connection()
                .query_row(
                    "SELECT COALESCE(jopacoin_balance, 0)
                     FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, guild_id],
                    |row| row.get(0),
                )
                .expect("read player balance")
        }

        fn package_ledger_rows(
            &self,
            discord_id: i64,
            guild_id: i64,
        ) -> Vec<(i64, String, Option<String>)> {
            let connection = self.connection();
            let mut statement = connection
                .prepare(
                    "SELECT delta, source, metadata
                     FROM economy_ledger_entries
                     WHERE guild_id = ?1 AND account_id = ?2 AND source = 'package_deal'
                     ORDER BY delta",
                )
                .unwrap();
            statement
                .query_map(params![guild_id, discord_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        }
    }

    #[test]
    fn test_create_deal() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 880)
            .unwrap();
        assert!(deal.id > 0);
        assert_eq!(deal.guild_id, GUILD_ID);
        assert_eq!(deal.buyer_discord_id, BUYER_ID);
        assert_eq!(deal.partner_discord_id, PARTNER_ID);
        assert_eq!(deal.games_remaining, 10);
        assert_eq!(deal.cost_paid, 880);
        assert!(deal.created_at > 0);
        assert!(deal.updated_at > 0);
    }

    #[test]
    fn test_create_deal_caps_duration_at_ten_games() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 25, 500)
            .unwrap();
        assert_eq!(deal.games_remaining, 10);
        assert_eq!(deal.games_added, 10);
        let purchases = fixture
            .repository
            .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, 0, i64::MAX)
            .unwrap();
        assert_eq!(
            purchases
                .iter()
                .map(|purchase| purchase.games_committed)
                .collect::<Vec<_>>(),
            [10]
        );
    }

    #[test]
    fn test_extend_deal_tops_up_to_ten_games() {
        let fixture = Fixture::new();
        let first = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 7, 500)
            .unwrap();
        let second = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 600)
            .unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.games_remaining, 10);
        assert_eq!(second.games_added, 3);
        assert_eq!(second.cost_paid, 1_100);
        let mut committed = fixture
            .repository
            .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, 0, i64::MAX)
            .unwrap()
            .into_iter()
            .map(|purchase| purchase.games_committed)
            .collect::<Vec<_>>();
        committed.sort_unstable();
        assert_eq!(committed, [3, 7]);
    }

    #[test]
    fn test_paid_purchase_at_cap_does_not_charge_or_record_purchase() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, GUILD_ID, 1_000);
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 1)
            .unwrap();
        let result = fixture
            .repository
            .purchase_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 1, 800)
            .unwrap();
        assert!(!result.success);
        assert_eq!(
            result.reason,
            Some(PackageDealPurchaseFailureReason::AlreadyFull)
        );
        assert_eq!(result.balance, 1_000);
        let deal = result.deal.unwrap();
        assert_eq!(deal.games_remaining, 10);
        assert_eq!(deal.games_added, 0);
        assert_eq!(fixture.balance(BUYER_ID, GUILD_ID), 1_000);
        let purchases = fixture
            .repository
            .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, 0, i64::MAX)
            .unwrap();
        assert_eq!(
            purchases
                .iter()
                .map(|purchase| (purchase.jc_spent, purchase.games_committed))
                .collect::<Vec<_>>(),
            [(1, 10)]
        );
        assert!(fixture.package_ledger_rows(BUYER_ID, GUILD_ID).is_empty());
    }

    #[test]
    fn test_concurrent_paid_top_ups_charge_once_and_stop_at_ten() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, GUILD_ID, 1_000);
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 7, 0)
            .unwrap();
        let repository = Arc::new(fixture.repository.clone());
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository
                        .purchase_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 3, 1, 100)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("purchase thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.success).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.reason == Some(PackageDealPurchaseFailureReason::AlreadyFull)
                })
                .count(),
            1
        );
        assert_eq!(fixture.balance(BUYER_ID, GUILD_ID), 900);
        let deals = repository.get_user_deals(Some(GUILD_ID), BUYER_ID).unwrap();
        assert_eq!(deals.len(), 1);
        assert_eq!(deals[0].games_remaining, 10);
        let mut purchases = repository
            .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, 0, i64::MAX)
            .unwrap()
            .into_iter()
            .map(|purchase| (purchase.jc_spent, purchase.games_committed))
            .collect::<Vec<_>>();
        purchases.sort_unstable();
        assert_eq!(purchases, [(0, 7), (100, 3)]);
        assert_eq!(fixture.package_ledger_rows(BUYER_ID, GUILD_ID).len(), 1);
    }

    #[test]
    fn test_concurrent_first_purchases_only_charge_introductory_price_once() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, GUILD_ID, 2_000);
        let repository = Arc::new(fixture.repository.clone());
        let barrier = Arc::new(Barrier::new(2));
        let handles = [200, 300]
            .into_iter()
            .map(|partner_id| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository
                        .purchase_deal(Some(GUILD_ID), BUYER_ID, partner_id, 10, 1, 800)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("purchase thread"))
            .collect::<Vec<_>>();
        assert!(results.iter().all(|result| result.success));
        let mut costs = results.iter().map(|result| result.cost).collect::<Vec<_>>();
        costs.sort_unstable();
        assert_eq!(costs, [1, 800]);
        assert_eq!(fixture.balance(BUYER_ID, GUILD_ID), 1_199);
        let deals = repository.get_user_deals(Some(GUILD_ID), BUYER_ID).unwrap();
        assert_eq!(deals.len(), 2);
        assert_eq!(
            deals
                .iter()
                .map(|deal| deal.partner_discord_id)
                .collect::<HashSet<_>>(),
            HashSet::from([200, 300])
        );
        assert!(deals.iter().all(|deal| deal.games_remaining <= 10));
        assert_eq!(fixture.package_ledger_rows(BUYER_ID, GUILD_ID).len(), 2);
    }

    #[test]
    fn test_cannot_deal_with_self() {
        let fixture = Fixture::new();
        let error = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, BUYER_ID, 10, 0)
            .expect_err("self deal must fail");
        assert!(matches!(error, PackageDealRepositoryError::SelfDeal));
        assert!(
            error
                .to_string()
                .contains("buyer and partner cannot be the same")
        );
    }

    #[test]
    fn test_get_active_deals_for_players() {
        let fixture = Fixture::new();
        for (buyer, partner) in [(100, 200), (300, 400), (100, 500)] {
            fixture
                .repository
                .create_or_extend_deal(Some(GUILD_ID), buyer, partner, 10, 0)
                .unwrap();
        }
        assert_eq!(
            fixture
                .repository
                .get_active_deals_for_players(Some(GUILD_ID), &[100, 200, 300, 400, 500])
                .unwrap()
                .len(),
            3
        );
        let deals = fixture
            .repository
            .get_active_deals_for_players(Some(GUILD_ID), &[100, 200])
            .unwrap();
        assert_eq!(deals.len(), 1);
        assert_eq!(deals[0].buyer_discord_id, 100);
        assert_eq!(deals[0].partner_discord_id, 200);
    }

    #[test]
    fn test_get_active_deals_excludes_expired() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 1, 0)
            .unwrap();
        fixture
            .repository
            .decrement_deals(Some(GUILD_ID), &[deal.id])
            .unwrap();
        assert!(
            fixture
                .repository
                .get_active_deals_for_players(Some(GUILD_ID), &[BUYER_ID, PARTNER_ID])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_get_user_deals() {
        let fixture = Fixture::new();
        for (buyer, partner) in [(100, 200), (100, 300), (200, 100)] {
            fixture
                .repository
                .create_or_extend_deal(Some(GUILD_ID), buyer, partner, 10, 0)
                .unwrap();
        }
        let deals = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        assert_eq!(deals.len(), 2);
        assert!(deals.iter().all(|deal| deal.buyer_discord_id == BUYER_ID));
    }

    #[test]
    fn test_decrement_deals() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 5, 0)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .decrement_deals(Some(GUILD_ID), &[deal.id])
                .unwrap(),
            1
        );
        let deals = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        assert_eq!(deals.len(), 1);
        assert_eq!(deals[0].games_remaining, 4);
    }

    #[test]
    fn test_decrement_multiple_deals() {
        let fixture = Fixture::new();
        let first = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, 200, 5, 0)
            .unwrap();
        let second = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, 300, 3, 0)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .decrement_deals(Some(GUILD_ID), &[first.id, second.id])
                .unwrap(),
            2
        );
        let games = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap()
            .into_iter()
            .map(|deal| (deal.partner_discord_id, deal.games_remaining))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(games.get(&200), Some(&4));
        assert_eq!(games.get(&300), Some(&2));
    }

    #[test]
    fn test_delete_expired_deals() {
        let fixture = Fixture::new();
        let first = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, 200, 1, 0)
            .unwrap();
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, 300, 5, 0)
            .unwrap();
        fixture
            .repository
            .decrement_deals(Some(GUILD_ID), &[first.id])
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .delete_expired_deals(Some(GUILD_ID))
                .unwrap(),
            1
        );
        let deals = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        assert_eq!(deals.len(), 1);
        assert_eq!(deals[0].partner_discord_id, 300);
    }

    #[test]
    fn test_guild_isolation() {
        let fixture = Fixture::new();
        let first = fixture
            .repository
            .create_or_extend_deal(Some(111), BUYER_ID, PARTNER_ID, 10, 0)
            .unwrap();
        let second = fixture
            .repository
            .create_or_extend_deal(Some(222), BUYER_ID, PARTNER_ID, 10, 0)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .get_user_deals(Some(111), BUYER_ID)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            fixture
                .repository
                .get_user_deals(Some(222), BUYER_ID)
                .unwrap()
                .len(),
            1
        );
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn test_null_guild_normalized() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(None, BUYER_ID, PARTNER_ID, 10, 0)
            .unwrap();
        assert_eq!(deal.guild_id, 0);
        assert_eq!(
            fixture
                .repository
                .get_user_deals(Some(0), BUYER_ID)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            fixture
                .repository
                .get_user_deals(None, BUYER_ID)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_purchase_log_survives_consumption_and_deletion() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 3, 90)
            .unwrap();
        for _ in 0..3 {
            fixture
                .repository
                .decrement_deals(Some(GUILD_ID), &[deal.id])
                .unwrap();
        }
        fixture
            .repository
            .delete_expired_deals(Some(GUILD_ID))
            .unwrap();
        assert!(
            fixture
                .repository
                .get_deals_involving_player(Some(GUILD_ID), BUYER_ID)
                .unwrap()
                .is_empty()
        );
        let purchases = fixture
            .repository
            .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, 0, i64::MAX)
            .unwrap();
        assert_eq!(purchases.len(), 1);
        assert_eq!(purchases[0].buyer_discord_id, BUYER_ID);
        assert_eq!(purchases[0].partner_discord_id, PARTNER_ID);
        assert_eq!(purchases[0].games_committed, 3);
        assert_eq!(purchases[0].jc_spent, 90);
    }

    #[test]
    fn test_purchase_log_time_window_filter() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 5, 40)
            .unwrap();
        let created = deal.created_at;
        assert_eq!(
            fixture
                .repository
                .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, created, created + 1,)
                .unwrap()
                .len(),
            1
        );
        assert!(
            fixture
                .repository
                .get_purchases_involving_player(
                    Some(GUILD_ID),
                    BUYER_ID,
                    created + 1,
                    created + 10,
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .repository
                .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, created - 10, created,)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_no_active_deals_costs_one_jopacoin() {
        let fixture = Fixture::new();
        let active = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        assert!(active.is_empty());
        assert_eq!(
            calculate_package_deal_cost(active.len() as i64, false, Some(1_500.0), Some(1_500.0)),
            PACKAGE_DEAL_NO_ACTIVE_COST
        );
        assert_eq!(PACKAGE_DEAL_NO_ACTIVE_COST, 1);
    }

    #[test]
    fn test_second_deal_costs_normal() {
        let fixture = Fixture::new();
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 1)
            .unwrap();
        let active = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        let is_extend = active.iter().any(|deal| deal.partner_discord_id == 300);
        assert!(!is_extend);
        let cost = calculate_package_deal_cost(
            active.len() as i64,
            is_extend,
            Some(1_500.0),
            Some(1_500.0),
        );
        let expected =
            SHOP_PACKAGE_DEAL_BASE_COST + (3_000.0 / SHOP_PACKAGE_DEAL_RATING_DIVISOR) as i64;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_extending_existing_deal_costs_normal() {
        let fixture = Fixture::new();
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 1)
            .unwrap();
        let active = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        let is_extend = active
            .iter()
            .any(|deal| deal.partner_discord_id == PARTNER_ID);
        assert!(is_extend);
        let cost = calculate_package_deal_cost(
            active.len() as i64,
            is_extend,
            Some(1_500.0),
            Some(1_500.0),
        );
        let expected =
            SHOP_PACKAGE_DEAL_BASE_COST + (3_000.0 / SHOP_PACKAGE_DEAL_RATING_DIVISOR) as i64;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_all_deals_expired_resets_to_one_jopacoin() {
        let fixture = Fixture::new();
        let deal = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 1, 0)
            .unwrap();
        fixture
            .repository
            .decrement_deals(Some(GUILD_ID), &[deal.id])
            .unwrap();
        let active = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        assert!(active.is_empty());
        assert_eq!(
            calculate_package_deal_cost(active.len() as i64, false, Some(1_500.0), Some(1_500.0)),
            1
        );
    }

    #[test]
    fn test_one_expired_one_active_still_paid() {
        let fixture = Fixture::new();
        let expired = fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, 200, 1, 0)
            .unwrap();
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, 300, 10, 500)
            .unwrap();
        fixture
            .repository
            .decrement_deals(Some(GUILD_ID), &[expired.id])
            .unwrap();
        let active = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap();
        assert_eq!(active.len(), 1);
        let is_extend = active.iter().any(|deal| deal.partner_discord_id == 400);
        let cost = calculate_package_deal_cost(
            active.len() as i64,
            is_extend,
            Some(1_500.0),
            Some(1_500.0),
        );
        let expected =
            SHOP_PACKAGE_DEAL_BASE_COST + (3_000.0 / SHOP_PACKAGE_DEAL_RATING_DIVISOR) as i64;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_rating_affects_cost() {
        let fixture = Fixture::new();
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 0)
            .unwrap();
        let active_count = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap()
            .len() as i64;
        let low = calculate_package_deal_cost(active_count, false, Some(1_000.0), Some(1_000.0));
        let high = calculate_package_deal_cost(active_count, false, Some(2_000.0), Some(2_000.0));
        assert!(high > low);
    }

    #[test]
    fn test_missing_ratings_default_to_1500() {
        let fixture = Fixture::new();
        fixture
            .repository
            .create_or_extend_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 0)
            .unwrap();
        let active_count = fixture
            .repository
            .get_user_deals(Some(GUILD_ID), BUYER_ID)
            .unwrap()
            .len() as i64;
        let cost = calculate_package_deal_cost(active_count, false, None, None);
        let expected =
            SHOP_PACKAGE_DEAL_BASE_COST + (3_000.0 / SHOP_PACKAGE_DEAL_RATING_DIVISOR) as i64;
        assert_eq!(cost, expected);
    }

    /// Extra migration invariant: a failed top-up rolls back debit and trigger ledger.
    #[test]
    fn write_failure_rolls_back_debit_ledger_and_purchase() {
        let fixture = Fixture::new();
        fixture.seed_player(BUYER_ID, GUILD_ID, 1_000);
        fixture
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_package_deal_insert
                 BEFORE INSERT ON package_deals
                 BEGIN
                     SELECT RAISE(ABORT, 'forced package deal failure');
                 END;",
            )
            .unwrap();
        let error = fixture
            .repository
            .purchase_deal(Some(GUILD_ID), BUYER_ID, PARTNER_ID, 10, 1, 800)
            .expect_err("deal write must abort");
        assert!(matches!(error, PackageDealRepositoryError::Sqlite(_)));
        assert!(error.to_string().contains("forced package deal failure"));
        assert_eq!(fixture.balance(BUYER_ID, GUILD_ID), 1_000);
        assert!(fixture.package_ledger_rows(BUYER_ID, GUILD_ID).is_empty());
        assert!(
            fixture
                .repository
                .get_purchases_involving_player(Some(GUILD_ID), BUYER_ID, 0, i64::MAX)
                .unwrap()
                .is_empty()
        );
    }
}
