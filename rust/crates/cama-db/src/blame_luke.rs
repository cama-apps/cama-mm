//! Existing-schema wallet adapter for the paid Blame Luke interaction.
//!
//! The adapter deliberately owns only the two balance transitions used by
//! `commands.blame_luke`: a conditional ten-JC debit and its compensating
//! delivery refund.  It never creates schema.  `BEGIN IMMEDIATE` serializes
//! the singleton economy-ledger context with the balance update, so concurrent
//! clickers cannot spend the same coins or leak audit metadata into another
//! writer.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

pub const BLAME_LUKE_COST: i64 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlameLukeChargeOutcome {
    Charged,
    Unregistered,
    InsufficientFunds,
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
