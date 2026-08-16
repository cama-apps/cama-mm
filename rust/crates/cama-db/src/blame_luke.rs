//! Existing-schema wallet adapter for the paid Blame Luke interaction.
//!
//! The adapter deliberately owns only the two balance transitions used by
//! `commands.blame_luke`: a conditional ten-JC debit and its compensating
//! delivery refund.  It never creates schema.  `BEGIN IMMEDIATE` serializes
//! the singleton economy-ledger context with the balance update, so concurrent
//! clickers cannot spend the same coins or leak audit metadata into another
//! writer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::open_runtime_connection;

pub const BLAME_LUKE_COST: i64 = 10;
const BLAME_LUKE_OPERATION_PREFIX: &str = "blame_luke:operation:";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlameLukeChargeOutcome {
    Charged,
    Unregistered,
    InsufficientFunds,
}

/// The result of an interaction-keyed charge attempt.  The existing
/// [`BlameLukeChargeOutcome`] remains the wallet-only API used by older
/// callers; this enum adds the durable outbox states needed by the live
/// interaction path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlameLukeOperationOutcome {
    Charged,
    AlreadyCharged,
    AlreadyDelivered,
    AlreadyRefunded,
    Unregistered,
    InsufficientFunds,
}

/// Exact Discord identity persisted with a paid Blame Luke operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlameLukeOperationIdentity {
    pub interaction_id: i64,
    pub user_id: i64,
    pub guild_id: i64,
    pub channel_id: Option<i64>,
    pub selected_reason_index: usize,
    pub user_display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlameLukeExistingOperation {
    Pending(BlameLukeOperationIdentity),
    Delivered,
    Refunded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlameLukeRecoverySummary {
    pub operations_recovered: usize,
    pub guilds_recovered: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum BlameLukeOperationStatus {
    Charged,
    Delivered,
    Refunded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PersistedBlameLukeOperation {
    interaction_id: i64,
    user_id: i64,
    guild_id: i64,
    channel_id: Option<i64>,
    selected_reason_index: usize,
    user_display_name: String,
    status: BlameLukeOperationStatus,
}

#[derive(Debug, Error)]
pub enum BlameLukeRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("Blame Luke reason index exceeds SQLite's signed integer range")]
    ReasonIndexOverflow,
    #[error("Spend amount cannot be negative")]
    NegativeSpendAmount,
    #[error("the charged Blame Luke player no longer exists")]
    RefundPlayerMissing,
    #[error("Blame Luke interaction identity does not match its durable operation")]
    OperationIdentityMismatch,
    #[error("invalid durable Blame Luke operation: {0}")]
    InvalidOperation(String),
    #[error("Blame Luke durable operation JSON failed: {0}")]
    OperationJson(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct BlameLukeRepository {
    path: PathBuf,
}

impl BlameLukeRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Atomically charge the clicker exactly ten JC and attach the selected
    /// reason index to the economy-ledger row produced by the existing trigger.
    pub fn charge(
        &self,
        user_id: i64,
        guild_id: i64,
        selected_reason_index: usize,
    ) -> Result<BlameLukeChargeOutcome, BlameLukeRepositoryError> {
        validate_spend_amount(BLAME_LUKE_COST)?;
        let related_id = i64::try_from(selected_reason_index)
            .map_err(|_| BlameLukeRepositoryError::ReasonIndexOverflow)?
            .to_string();
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        set_ledger_context(
            &transaction,
            "blame_luke",
            user_id,
            Some(&related_id),
            "Blame Luke animation",
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) >= ?1",
            params![BLAME_LUKE_COST, user_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;

        let outcome = if changed == 1 {
            transaction.execute(
                "UPDATE players
                 SET lowest_balance_ever = jopacoin_balance
                 WHERE discord_id = ?1 AND guild_id = ?2
                   AND (lowest_balance_ever IS NULL
                        OR jopacoin_balance < lowest_balance_ever)",
                params![user_id, guild_id],
            )?;
            BlameLukeChargeOutcome::Charged
        } else if player_exists(&transaction, user_id, guild_id)? {
            BlameLukeChargeOutcome::InsufficientFunds
        } else {
            BlameLukeChargeOutcome::Unregistered
        };
        transaction.commit()?;
        Ok(outcome)
    }

    /// Check eligibility without changing the wallet.  The provider uses this
    /// before rendering so insufficient or unregistered clicks keep their
    /// historical no-render behavior.
    pub fn eligibility(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<BlameLukeChargeOutcome, BlameLukeRepositoryError> {
        validate_spend_amount(BLAME_LUKE_COST)?;
        let connection = open_runtime_connection(&self.path)?;
        let balance = connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0)
                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![user_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(match balance {
            None => BlameLukeChargeOutcome::Unregistered,
            Some(balance) if balance < BLAME_LUKE_COST => BlameLukeChargeOutcome::InsufficientFunds,
            Some(_) => BlameLukeChargeOutcome::Charged,
        })
    }

    /// Load an interaction's durable state before selecting a random reason.
    /// A retried Discord delivery must render the original snapshot instead
    /// of generating a new identity and accidentally refusing its receipt.
    pub fn existing_operation(
        &self,
        interaction_id: i64,
    ) -> Result<Option<BlameLukeExistingOperation>, BlameLukeRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let key = operation_key(interaction_id);
        let mut statement = connection
            .prepare("SELECT guild_id,value FROM app_kv WHERE key=?1 ORDER BY guild_id")?;
        let values = statement
            .query_map([key], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() > 1 {
            return Err(BlameLukeRepositoryError::InvalidOperation(
                "multiple guilds share a Blame Luke interaction key".to_owned(),
            ));
        }
        let Some((stored_guild_id, raw)) = values.first() else {
            return Ok(None);
        };
        let operation: PersistedBlameLukeOperation = serde_json::from_str(raw)?;
        if *stored_guild_id != operation.guild_id {
            return Err(BlameLukeRepositoryError::InvalidOperation(
                "Blame Luke operation guild does not match its app_kv partition".to_owned(),
            ));
        }
        if operation.interaction_id != interaction_id {
            return Err(BlameLukeRepositoryError::InvalidOperation(
                "interaction key does not match payload".to_owned(),
            ));
        }
        let identity = BlameLukeOperationIdentity {
            interaction_id: operation.interaction_id,
            user_id: operation.user_id,
            guild_id: operation.guild_id,
            channel_id: operation.channel_id,
            selected_reason_index: operation.selected_reason_index,
            user_display_name: operation.user_display_name,
        };
        Ok(Some(match operation.status {
            BlameLukeOperationStatus::Charged => BlameLukeExistingOperation::Pending(identity),
            BlameLukeOperationStatus::Delivered => BlameLukeExistingOperation::Delivered,
            BlameLukeOperationStatus::Refunded => BlameLukeExistingOperation::Refunded,
        }))
    }

    /// Atomically charge a clicker and persist the interaction's pending
    /// delivery operation in the same SQLite transaction.  A process death at
    /// any later point therefore leaves either no debit or a READY-recoverable
    /// `Charged` outbox row, never an untracked charge.
    pub fn charge_for_interaction(
        &self,
        identity: &BlameLukeOperationIdentity,
    ) -> Result<BlameLukeOperationOutcome, BlameLukeRepositoryError> {
        validate_spend_amount(BLAME_LUKE_COST)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = operation_key(identity.interaction_id);
        if let Some(existing) = load_operation(&transaction, &key)? {
            if !same_identity(&existing, identity) {
                return Err(BlameLukeRepositoryError::OperationIdentityMismatch);
            }
            transaction.commit()?;
            return Ok(match existing.status {
                BlameLukeOperationStatus::Charged => BlameLukeOperationOutcome::AlreadyCharged,
                BlameLukeOperationStatus::Delivered => BlameLukeOperationOutcome::AlreadyDelivered,
                BlameLukeOperationStatus::Refunded => BlameLukeOperationOutcome::AlreadyRefunded,
            });
        }

        let related_id = i64::try_from(identity.selected_reason_index)
            .map_err(|_| BlameLukeRepositoryError::ReasonIndexOverflow)?
            .to_string();
        set_ledger_context(
            &transaction,
            "blame_luke",
            identity.user_id,
            Some(&related_id),
            "Blame Luke animation",
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) >= ?1",
            params![BLAME_LUKE_COST, identity.user_id, identity.guild_id],
        )?;
        clear_ledger_context(&transaction)?;

        let outcome = if changed == 1 {
            transaction.execute(
                "UPDATE players
                 SET lowest_balance_ever = jopacoin_balance
                 WHERE discord_id = ?1 AND guild_id = ?2
                   AND (lowest_balance_ever IS NULL
                        OR jopacoin_balance < lowest_balance_ever)",
                params![identity.user_id, identity.guild_id],
            )?;
            let operation = PersistedBlameLukeOperation {
                interaction_id: identity.interaction_id,
                user_id: identity.user_id,
                guild_id: identity.guild_id,
                channel_id: identity.channel_id,
                selected_reason_index: identity.selected_reason_index,
                user_display_name: identity.user_display_name.clone(),
                status: BlameLukeOperationStatus::Charged,
            };
            transaction.execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)",
                params![identity.guild_id, key, serde_json::to_string(&operation)?],
            )?;
            BlameLukeOperationOutcome::Charged
        } else if player_exists(&transaction, identity.user_id, identity.guild_id)? {
            BlameLukeOperationOutcome::InsufficientFunds
        } else {
            BlameLukeOperationOutcome::Unregistered
        };
        transaction.commit()?;
        Ok(outcome)
    }

    /// Record the Discord followup receipt.  This update is idempotent and is
    /// deliberately after the network send, so an ambiguous send remains a
    /// pending operation that READY can safely reconcile with a refund.
    pub fn mark_interaction_delivered(
        &self,
        interaction_id: i64,
    ) -> Result<bool, BlameLukeRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = operation_key(interaction_id);
        let Some((guild_id, raw, mut operation)) = load_operation_with_raw(&transaction, &key)?
        else {
            transaction.commit()?;
            return Ok(false);
        };
        if operation.interaction_id != interaction_id {
            return Err(BlameLukeRepositoryError::InvalidOperation(
                "interaction key does not match payload".to_owned(),
            ));
        }
        if operation.status != BlameLukeOperationStatus::Charged {
            transaction.commit()?;
            return Ok(false);
        }
        operation.status = BlameLukeOperationStatus::Delivered;
        let updated = transaction.execute(
            "UPDATE app_kv SET value=?1 WHERE guild_id=?2 AND key=?3 AND value=?4",
            params![serde_json::to_string(&operation)?, guild_id, key, raw],
        )?;
        transaction.commit()?;
        Ok(updated == 1)
    }

    /// Refund one interaction at most once, using the exact identity recorded
    /// alongside the debit.  This closes repeated delivery failures and makes
    /// READY reconciliation safe to rerun after a restart.
    pub fn refund_interaction(
        &self,
        interaction_id: i64,
    ) -> Result<bool, BlameLukeRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = operation_key(interaction_id);
        let Some((guild_id, raw, mut operation)) = load_operation_with_raw(&transaction, &key)?
        else {
            transaction.commit()?;
            return Ok(false);
        };
        if operation.interaction_id != interaction_id {
            return Err(BlameLukeRepositoryError::InvalidOperation(
                "interaction key does not match payload".to_owned(),
            ));
        }
        if operation.status != BlameLukeOperationStatus::Charged {
            transaction.commit()?;
            return Ok(false);
        }
        set_ledger_context(
            &transaction,
            "blame_luke_refund",
            operation.user_id,
            None,
            "Blame Luke delivery refund",
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![BLAME_LUKE_COST, operation.user_id, operation.guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        if changed != 1 {
            return Err(BlameLukeRepositoryError::RefundPlayerMissing);
        }
        operation.status = BlameLukeOperationStatus::Refunded;
        let updated = transaction.execute(
            "UPDATE app_kv SET value=?1 WHERE guild_id=?2 AND key=?3 AND value=?4",
            params![serde_json::to_string(&operation)?, guild_id, key, raw],
        )?;
        if updated != 1 {
            return Err(BlameLukeRepositoryError::InvalidOperation(
                "Blame Luke operation changed before refund commit".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Refund all charged operations left by an interrupted provider.  Each
    /// operation gets its own idempotent transaction so one malformed/missing
    /// player cannot make the rest of the durable queue invisible forever.
    pub fn recover_pending_interactions(
        &self,
    ) -> Result<BlameLukeRecoverySummary, BlameLukeRepositoryError> {
        self.recover_pending_interactions_scoped(None)
    }

    pub fn recover_pending_interactions_for_guilds(
        &self,
        guild_ids: &BTreeSet<i64>,
    ) -> Result<BlameLukeRecoverySummary, BlameLukeRepositoryError> {
        self.recover_pending_interactions_scoped(Some(guild_ids))
    }

    fn recover_pending_interactions_scoped(
        &self,
        guild_ids: Option<&BTreeSet<i64>>,
    ) -> Result<BlameLukeRecoverySummary, BlameLukeRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT value FROM app_kv
             WHERE key GLOB 'blame_luke:operation:*'",
        )?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        let mut recovered = 0;
        let mut recovered_guilds = std::collections::BTreeSet::new();
        let mut first_error = None;
        for raw in values {
            let operation: PersistedBlameLukeOperation = match serde_json::from_str(&raw) {
                Ok(operation) => operation,
                Err(error) => {
                    first_error.get_or_insert(BlameLukeRepositoryError::OperationJson(error));
                    continue;
                }
            };
            if guild_ids.is_some_and(|guild_ids| !guild_ids.contains(&operation.guild_id)) {
                continue;
            }
            if operation.status == BlameLukeOperationStatus::Charged {
                match self.refund_interaction(operation.interaction_id) {
                    Ok(true) => {
                        recovered += 1;
                        recovered_guilds.insert(operation.guild_id);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(BlameLukeRecoverySummary {
            operations_recovered: recovered,
            guilds_recovered: recovered_guilds.len(),
        })
    }

    /// Compensate a successful charge after defer, render, or delivery failure.
    pub fn refund(&self, user_id: i64, guild_id: i64) -> Result<(), BlameLukeRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        set_ledger_context(
            &transaction,
            "blame_luke_refund",
            user_id,
            None,
            "Blame Luke delivery refund",
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![BLAME_LUKE_COST, user_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        if changed != 1 {
            return Err(BlameLukeRepositoryError::RefundPlayerMissing);
        }
        transaction.commit()?;
        Ok(())
    }
}

fn operation_key(interaction_id: i64) -> String {
    format!("{BLAME_LUKE_OPERATION_PREFIX}{interaction_id}")
}

fn same_identity(
    operation: &PersistedBlameLukeOperation,
    identity: &BlameLukeOperationIdentity,
) -> bool {
    operation.interaction_id == identity.interaction_id
        && operation.user_id == identity.user_id
        && operation.guild_id == identity.guild_id
        && operation.channel_id == identity.channel_id
        && operation.selected_reason_index == identity.selected_reason_index
        && operation.user_display_name == identity.user_display_name
}

fn load_operation(
    transaction: &Transaction<'_>,
    key: &str,
) -> Result<Option<PersistedBlameLukeOperation>, BlameLukeRepositoryError> {
    let mut statement =
        transaction.prepare("SELECT guild_id,value FROM app_kv WHERE key=?1 ORDER BY guild_id")?;
    let values = statement
        .query_map([key], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let Some((stored_guild_id, raw)) = values.first() else {
        return Ok(None);
    };
    if values.len() > 1 {
        return Err(BlameLukeRepositoryError::InvalidOperation(
            "multiple guilds share a Blame Luke interaction key".to_owned(),
        ));
    }
    let operation: PersistedBlameLukeOperation = serde_json::from_str(raw)?;
    if *stored_guild_id != operation.guild_id {
        return Err(BlameLukeRepositoryError::InvalidOperation(
            "Blame Luke operation guild does not match its app_kv partition".to_owned(),
        ));
    }
    Ok(Some(operation))
}

fn load_operation_with_raw(
    transaction: &Transaction<'_>,
    key: &str,
) -> Result<Option<(i64, String, PersistedBlameLukeOperation)>, BlameLukeRepositoryError> {
    let mut statement =
        transaction.prepare("SELECT guild_id,value FROM app_kv WHERE key=?1 ORDER BY guild_id")?;
    let rows = statement
        .query_map([key], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let Some((guild_id, raw)) = rows.into_iter().next() else {
        return Ok(None);
    };
    if transaction.query_row("SELECT COUNT(*) FROM app_kv WHERE key=?1", [key], |row| {
        row.get::<_, i64>(0)
    })? > 1
    {
        return Err(BlameLukeRepositoryError::InvalidOperation(
            "multiple guilds share a Blame Luke interaction key".to_owned(),
        ));
    }
    let operation: PersistedBlameLukeOperation = serde_json::from_str(&raw)?;
    if guild_id != operation.guild_id {
        return Err(BlameLukeRepositoryError::InvalidOperation(
            "Blame Luke operation guild does not match its app_kv partition".to_owned(),
        ));
    }
    Ok(Some((guild_id, raw, operation)))
}

/// Match `PlayerService.try_spend`'s caller-side guard before any repository
/// debit. The live Blame Luke path supplies the fixed positive cost, while the
/// public helper keeps the shared spending contract explicit and testable.
pub fn validate_spend_amount(amount: i64) -> Result<(), BlameLukeRepositoryError> {
    if amount < 0 {
        return Err(BlameLukeRepositoryError::NegativeSpendAmount);
    }
    Ok(())
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: i64,
    related_id: Option<&str>,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason
         ) VALUES (1, ?1, ?2, 'blame_luke', ?3, ?4)",
        params![source, actor_id, related_id, reason],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn player_exists(
    transaction: &Transaction<'_>,
    user_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    Ok(transaction
        .query_row(
            "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![user_id, guild_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use crate::schema_manager::initialize_or_migrate;
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;

    const USER: i64 = 42;
    const GUILD: i64 = 123;

    struct Fixture {
        _directory: TempDir,
        path: PathBuf,
    }

    impl Fixture {
        fn migrated() -> Self {
            let directory = tempfile::tempdir().expect("temporary database directory");
            let path = directory.path().join("cama.db");
            initialize_or_migrate(&path).expect("run Rust migration authority");
            Self {
                _directory: directory,
                path,
            }
        }

        fn connection(&self) -> Connection {
            let connection = Connection::open(&self.path).expect("open fixture database");
            connection
                .pragma_update(None, "foreign_keys", false)
                .expect("match production foreign-key mode");
            connection
        }

        fn register(&self, balance: i64) {
            self.connection()
                .execute(
                    "INSERT INTO players (
                         discord_id, guild_id, discord_username, jopacoin_balance
                     ) VALUES (?1, ?2, 'blame-luke-fixture', ?3)",
                    params![USER, GUILD, balance],
                )
                .expect("register fixture player");
            self.connection()
                .execute("DELETE FROM economy_ledger_entries", [])
                .expect("clear opening ledger entry");
        }

        fn balance(&self) -> i64 {
            self.connection()
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![USER, GUILD],
                    |row| row.get(0),
                )
                .expect("read fixture balance")
        }

        fn ledger(&self) -> Vec<(i64, String, Option<String>)> {
            let connection = self.connection();
            let mut statement = connection
                .prepare(
                    "SELECT delta, source, related_id
                     FROM economy_ledger_entries ORDER BY ledger_id",
                )
                .expect("prepare ledger query");
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("query ledger")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect ledger")
        }
    }

    #[test]
    fn migrated_sqlite_charge_and_refund_preserve_exact_ledger_context() {
        let fixture = Fixture::migrated();
        fixture.register(25);
        let repository = BlameLukeRepository::new(&fixture.path);

        assert_eq!(
            repository.charge(USER, GUILD, 3).expect("charge"),
            BlameLukeChargeOutcome::Charged
        );
        assert_eq!(fixture.balance(), 15);
        repository.refund(USER, GUILD).expect("refund");
        assert_eq!(fixture.balance(), 25);

        let connection = fixture.connection();
        let mut statement = connection
            .prepare(
                "SELECT delta, source, actor_id, related_type, related_id, reason
                 FROM economy_ledger_entries ORDER BY ledger_id",
            )
            .expect("prepare ledger query");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .expect("query ledger")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ledger rows");
        assert_eq!(
            rows,
            vec![
                (
                    -10,
                    "blame_luke".to_owned(),
                    Some(USER),
                    Some("blame_luke".to_owned()),
                    Some("3".to_owned()),
                    Some("Blame Luke animation".to_owned()),
                ),
                (
                    10,
                    "blame_luke_refund".to_owned(),
                    Some(USER),
                    Some("blame_luke".to_owned()),
                    None,
                    Some("Blame Luke delivery refund".to_owned()),
                ),
            ]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count ledger context"),
            0
        );
    }

    #[test]
    fn charge_distinguishes_unregistered_from_insufficient_without_a_write() {
        let fixture = Fixture::migrated();
        let repository = BlameLukeRepository::new(&fixture.path);
        assert_eq!(
            repository.charge(USER, GUILD, 0).expect("unregistered"),
            BlameLukeChargeOutcome::Unregistered
        );
        fixture.register(9);
        assert_eq!(
            repository.charge(USER, GUILD, 0).expect("insufficient"),
            BlameLukeChargeOutcome::InsufficientFunds
        );
        assert_eq!(fixture.balance(), 9);
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count ledger entries"),
            0
        );
    }

    #[test]
    fn concurrent_clicks_cannot_double_spend_the_same_ten_jc() {
        let fixture = Fixture::migrated();
        fixture.register(10);
        let repository = Arc::new(BlameLukeRepository::new(&fixture.path));
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|index| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository.charge(USER, GUILD, index).expect("race charge")
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().expect("charge thread"))
            .collect::<Vec<_>>();

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == BlameLukeChargeOutcome::Charged)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == BlameLukeChargeOutcome::InsufficientFunds)
                .count(),
            1
        );
        assert_eq!(fixture.balance(), 0);
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE source = 'blame_luke' AND delta = -10",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count successful charges"),
            1
        );
    }

    fn operation(interaction_id: i64) -> BlameLukeOperationIdentity {
        BlameLukeOperationIdentity {
            interaction_id,
            user_id: USER,
            guild_id: GUILD,
            channel_id: Some(999),
            selected_reason_index: 3,
            user_display_name: "Exact Clicker".to_owned(),
        }
    }

    #[test]
    fn interaction_charge_persists_exact_identity_and_delivery_receipt() {
        let fixture = Fixture::migrated();
        fixture.register(25);
        let repository = BlameLukeRepository::new(&fixture.path);
        let identity = operation(777);

        assert_eq!(
            repository
                .charge_for_interaction(&identity)
                .expect("durable charge"),
            BlameLukeOperationOutcome::Charged
        );
        assert_eq!(fixture.balance(), 15);
        assert_eq!(
            repository
                .charge_for_interaction(&identity)
                .expect("idempotent charge"),
            BlameLukeOperationOutcome::AlreadyCharged
        );
        let mut cross_guild_identity = identity.clone();
        cross_guild_identity.guild_id = GUILD + 1;
        assert!(matches!(
            repository.charge_for_interaction(&cross_guild_identity),
            Err(BlameLukeRepositoryError::OperationIdentityMismatch)
        ));
        assert_eq!(fixture.balance(), 15);
        let operation_json: String = fixture
            .connection()
            .query_row(
                "SELECT value FROM app_kv
                 WHERE guild_id=?1 AND key='blame_luke:operation:777'",
                [GUILD],
                |row| row.get(0),
            )
            .expect("read durable operation");
        assert!(operation_json.contains("\"interaction_id\":777"));
        assert!(operation_json.contains("\"user_id\":42"));
        assert!(operation_json.contains("\"guild_id\":123"));
        assert!(operation_json.contains("\"channel_id\":999"));
        assert!(operation_json.contains("\"selected_reason_index\":3"));
        assert!(operation_json.contains("\"user_display_name\":\"Exact Clicker\""));
        assert!(
            repository
                .mark_interaction_delivered(777)
                .expect("delivery receipt")
        );
        assert!(
            !repository
                .mark_interaction_delivered(777)
                .expect("idempotent delivery receipt")
        );
        assert_eq!(
            repository
                .charge_for_interaction(&identity)
                .expect("delivered replay"),
            BlameLukeOperationOutcome::AlreadyDelivered
        );
        assert!(
            !repository
                .refund_interaction(777)
                .expect("delivered operation is not refundable")
        );
        assert_eq!(fixture.balance(), 15);
    }

    #[test]
    fn pending_interaction_is_refunded_once_by_restart_recovery() {
        let fixture = Fixture::migrated();
        fixture.register(25);
        let repository = BlameLukeRepository::new(&fixture.path);
        repository
            .charge_for_interaction(&operation(778))
            .expect("durable charge before simulated crash");
        assert_eq!(fixture.balance(), 15);

        let restarted = BlameLukeRepository::new(&fixture.path);
        assert_eq!(
            restarted
                .recover_pending_interactions()
                .expect("READY pending refund"),
            BlameLukeRecoverySummary {
                operations_recovered: 1,
                guilds_recovered: 1,
            }
        );
        assert_eq!(fixture.balance(), 25);
        assert_eq!(
            restarted
                .recover_pending_interactions()
                .expect("repeat READY recovery"),
            BlameLukeRecoverySummary::default()
        );
        assert_eq!(fixture.balance(), 25);
        assert_eq!(
            fixture.ledger(),
            [
                (-10, "blame_luke".to_owned(), Some("3".to_owned())),
                (10, "blame_luke_refund".to_owned(), None),
            ]
        );
    }

    #[test]
    fn recovery_reports_distinct_guilds_for_multiple_pending_interactions() {
        let fixture = Fixture::migrated();
        fixture.register(30);
        let repository = BlameLukeRepository::new(&fixture.path);
        repository
            .charge_for_interaction(&operation(779))
            .expect("first pending charge");
        repository
            .charge_for_interaction(&operation(780))
            .expect("second pending charge");

        let summary = repository
            .recover_pending_interactions()
            .expect("recover both pending operations");
        assert_eq!(
            summary,
            BlameLukeRecoverySummary {
                operations_recovered: 2,
                guilds_recovered: 1,
            }
        );
        assert_eq!(fixture.balance(), 30);
    }

    #[test]
    fn malformed_outbox_row_does_not_strand_later_valid_pending_charge() {
        let fixture = Fixture::migrated();
        fixture.register(25);
        let repository = BlameLukeRepository::new(&fixture.path);
        repository
            .charge_for_interaction(&operation(781))
            .expect("valid pending charge");
        fixture
            .connection()
            .execute(
                "INSERT INTO app_kv(guild_id,key,value)
                 VALUES (?1,'blame_luke:operation:bad','not-json')",
                [GUILD],
            )
            .expect("inject malformed durable row");

        let error = repository
            .recover_pending_interactions()
            .expect_err("surface malformed row after processing all operations");
        assert!(matches!(error, BlameLukeRepositoryError::OperationJson(_)));
        assert_eq!(fixture.balance(), 25);
        fixture
            .connection()
            .execute(
                "DELETE FROM app_kv WHERE guild_id=?1 AND key='blame_luke:operation:bad'",
                [GUILD],
            )
            .expect("remove malformed durable row");
        assert_eq!(
            repository
                .recover_pending_interactions()
                .expect("repeat recovery"),
            BlameLukeRecoverySummary::default()
        );
    }

    #[test]
    fn operation_insert_failure_rolls_back_charge_ledger_and_context() {
        let fixture = Fixture::migrated();
        fixture.register(25);
        fixture
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_blame_luke_operation_insert
                 BEFORE INSERT ON app_kv
                 WHEN NEW.key GLOB 'blame_luke:operation:*'
                 BEGIN SELECT RAISE(ABORT, 'injected operation outbox failure'); END;",
            )
            .expect("install operation failure trigger");
        let repository = BlameLukeRepository::new(&fixture.path);
        let error = repository
            .charge_for_interaction(&operation(783))
            .expect_err("injected outbox failure");
        assert!(
            error
                .to_string()
                .contains("injected operation outbox failure")
        );
        assert_eq!(fixture.balance(), 25);
        assert!(fixture.ledger().is_empty());
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count ledger context"),
            0
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM app_kv WHERE key='blame_luke:operation:783'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count failed operation"),
            0
        );
    }

    #[test]
    fn concurrent_same_interaction_creates_one_charge_and_one_pending_row() {
        let fixture = Fixture::migrated();
        fixture.register(10);
        let repository = Arc::new(BlameLukeRepository::new(&fixture.path));
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository
                        .charge_for_interaction(&operation(784))
                        .expect("same-interaction charge")
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().expect("same-interaction thread"))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == BlameLukeOperationOutcome::Charged)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == BlameLukeOperationOutcome::AlreadyCharged)
                .count(),
            1
        );
        assert_eq!(fixture.balance(), 0);
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE source='blame_luke' AND delta=-10",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count same-interaction charges"),
            1
        );
    }

    #[test]
    fn terminal_refund_update_failure_rolls_back_credit_and_keeps_pending() {
        let fixture = Fixture::migrated();
        fixture.register(25);
        let repository = BlameLukeRepository::new(&fixture.path);
        repository
            .charge_for_interaction(&operation(785))
            .expect("pending charge");
        fixture
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_blame_luke_refund_update
                 BEFORE UPDATE ON app_kv
                 WHEN NEW.key='blame_luke:operation:785'
                   AND NEW.value LIKE '%Refunded%'
                 BEGIN SELECT RAISE(ABORT, 'injected refund receipt failure'); END;",
            )
            .expect("install refund receipt failure trigger");
        let error = repository
            .refund_interaction(785)
            .expect_err("injected refund receipt failure");
        assert!(
            error
                .to_string()
                .contains("injected refund receipt failure")
        );
        assert_eq!(fixture.balance(), 15);
        assert_eq!(
            fixture.ledger(),
            [(-10, "blame_luke".to_owned(), Some("3".to_owned()))]
        );
        fixture
            .connection()
            .execute("DROP TRIGGER fail_blame_luke_refund_update", [])
            .expect("remove refund receipt failure trigger");
        assert!(
            repository
                .refund_interaction(785)
                .expect("retry refund after restart")
        );
        assert_eq!(fixture.balance(), 25);
    }

    #[test]
    fn test_player_service_try_spend_rejects_negative_amount() {
        assert!(matches!(
            validate_spend_amount(-1),
            Err(BlameLukeRepositoryError::NegativeSpendAmount)
        ));
        assert!(validate_spend_amount(0).is_ok());
        assert!(validate_spend_amount(BLAME_LUKE_COST).is_ok());
    }
}
