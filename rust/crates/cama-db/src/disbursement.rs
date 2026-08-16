//! Durable existing-schema Reserve proposal and ballot operations.
//!
//! Python's `DisburseService` keeps policy (eligibility and distribution
//! calculations) above this boundary.  This repository owns the state
//! machine: reserve locking, latest-vote replacement, vote-history archival,
//! and all-or-nothing proposal completion.  Every mutating operation uses a
//! SQLite immediate transaction so a restart cannot strand or duplicate funds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;
use thiserror::Error;

use crate::open_runtime_connection;

pub const DISBURSE_METHODS: [&str; 10] = [
    "even",
    "proportional",
    "neediest",
    "stimulus",
    "lottery",
    "social_security",
    "richest",
    "burn",
    "next_match_pot",
    "cancel",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisbursementProposal {
    pub guild_id: i64,
    pub proposal_id: i64,
    pub message_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub fund_amount: i64,
    pub quorum_required: i64,
    pub status: String,
    pub created_at: Option<String>,
    pub votes: BTreeMap<String, i64>,
}

impl DisbursementProposal {
    #[must_use]
    pub fn total_votes(&self) -> i64 {
        self.votes.values().sum()
    }

    #[must_use]
    pub fn quorum_reached(&self) -> bool {
        self.total_votes() >= self.quorum_required
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisbursementVote {
    pub discord_id: i64,
    pub vote_method: String,
    pub voted_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisbursementHistory {
    pub id: i64,
    pub guild_id: i64,
    pub disbursed_at: i64,
    pub total_amount: i64,
    pub method: String,
    pub recipient_count: i64,
    pub recipients: Vec<(i64, i64)>,
}

#[derive(Debug, Error)]
pub enum DisbursementRepositoryError {
    #[error("A disbursement vote is already active.")]
    ActiveProposal,
    #[error("No active proposal")]
    NoActiveProposal,
    #[error("Invalid vote method: {0}")]
    InvalidMethod(String),
    #[error("Insufficient funds. Available: {available}, minimum required: {minimum}")]
    InsufficientFunds { available: i64, minimum: i64 },
    #[error("disbursement amount arithmetic overflowed")]
    AmountOverflow,
    #[error("Disbursement aborted: recipient {0} is not registered")]
    MissingRecipient(i64),
    #[error("Insufficient funds in nonprofit. Available: {available}, needed: {needed}")]
    InsufficientDistributionFunds { available: i64, needed: i64 },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct DisbursementRepository {
    path: PathBuf,
}

impl DisbursementRepository {
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

    pub fn get_active_proposal(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<DisbursementProposal>, DisbursementRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let Some(row) = connection
            .query_row(
                "SELECT guild_id, proposal_id, message_id, channel_id, fund_amount,
                        quorum_required, status, created_at
                 FROM disburse_proposals WHERE guild_id = ?1 AND status = 'active'",
                [guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut proposal = DisbursementProposal {
            guild_id: row.0,
            proposal_id: row.1,
            message_id: row.2,
            channel_id: row.3,
            fund_amount: row.4,
            quorum_required: row.5,
            status: row.6,
            created_at: row.7,
            votes: zero_votes(),
        };
        let mut statement = connection.prepare(
            "SELECT vote_method, COUNT(*) FROM disburse_votes
             WHERE guild_id = ?1 AND proposal_id = ?2 GROUP BY vote_method",
        )?;
        let rows = statement.query_map(params![guild_id, proposal.proposal_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (method, count) = row?;
            if let Some(slot) = proposal.votes.get_mut(&method) {
                *slot = count;
            }
        }
        Ok(Some(proposal))
    }

    /// Lock the current Reserve amount and create one active proposal in the
    /// same transaction.  `quorum_required` is computed by the policy layer.
    pub fn create_proposal_atomic(
        &self,
        guild_id: Option<i64>,
        proposal_id: i64,
        minimum_fund: i64,
        quorum_required: i64,
    ) -> Result<DisbursementProposal, DisbursementRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction
            .query_row(
                "SELECT 1 FROM disburse_proposals
                 WHERE guild_id = ?1 AND status = 'active'",
                [guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(DisbursementRepositoryError::ActiveProposal);
        }
        let available = nonprofit_total(&transaction, guild_id)?;
        if available < minimum_fund {
            return Err(DisbursementRepositoryError::InsufficientFunds {
                available,
                minimum: minimum_fund,
            });
        }
        let fund_amount = available;
        let updated = available
            .checked_sub(fund_amount)
            .ok_or(DisbursementRepositoryError::AmountOverflow)?;
        with_ledger_context(
            &transaction,
            proposal_id,
            "Jopacoin Reserve funds locked for allocation vote",
            || set_nonprofit_total(&transaction, guild_id, updated),
        )?;
        transaction.execute(
            "INSERT INTO disburse_proposals
                 (guild_id, proposal_id, fund_amount, quorum_required, status)
             VALUES (?1, ?2, ?3, ?4, 'active')",
            params![guild_id, proposal_id, fund_amount, quorum_required.max(1)],
        )?;
        transaction.execute(
            "DELETE FROM disburse_votes WHERE guild_id = ?1 AND proposal_id != ?2",
            params![guild_id, proposal_id],
        )?;
        transaction.commit()?;
        Ok(DisbursementProposal {
            guild_id,
            proposal_id,
            message_id: None,
            channel_id: None,
            fund_amount,
            quorum_required: quorum_required.max(1),
            status: "active".to_owned(),
            created_at: None,
            votes: zero_votes(),
        })
    }

    pub fn set_proposal_message(
        &self,
        guild_id: Option<i64>,
        message_id: i64,
        channel_id: i64,
    ) -> Result<bool, DisbursementRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE disburse_proposals SET message_id = ?1, channel_id = ?2
             WHERE guild_id = ?3 AND status = 'active'",
            params![message_id, channel_id, Self::normalize_guild_id(guild_id)],
        )? == 1)
    }

    pub fn add_vote(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        method: &str,
    ) -> Result<DisbursementProposal, DisbursementRepositoryError> {
        validate_method(method)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let proposal_id = transaction
            .query_row(
                "SELECT proposal_id FROM disburse_proposals
                 WHERE guild_id = ?1 AND status = 'active'",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(DisbursementRepositoryError::NoActiveProposal)?;
        let now = unix_seconds();
        transaction.execute(
            "INSERT INTO disburse_votes(guild_id, proposal_id, discord_id, vote_method, voted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(guild_id, proposal_id, discord_id) DO UPDATE SET
                 vote_method = excluded.vote_method, voted_at = excluded.voted_at",
            params![guild_id, proposal_id, discord_id, method, now],
        )?;
        transaction.commit()?;
        self.get_active_proposal(Some(guild_id))?
            .ok_or(DisbursementRepositoryError::NoActiveProposal)
    }

    pub fn individual_votes(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<DisbursementVote>, DisbursementRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let Some(proposal_id) = connection
            .query_row(
                "SELECT proposal_id FROM disburse_proposals
                 WHERE guild_id = ?1 AND status = 'active'",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(Vec::new());
        };
        let mut statement = connection.prepare(
            "SELECT discord_id, vote_method, voted_at FROM disburse_votes
             WHERE guild_id = ?1 AND proposal_id = ?2 ORDER BY voted_at ASC, discord_id ASC",
        )?;
        let rows = statement.query_map(params![guild_id, proposal_id], |row| {
            Ok(DisbursementVote {
                discord_id: row.get(0)?,
                vote_method: row.get(1)?,
                voted_at: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn reset_and_return_fund_atomic(
        &self,
        guild_id: Option<i64>,
        outcome: &str,
    ) -> Result<bool, DisbursementRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((proposal_id, fund_amount)) = transaction
            .query_row(
                "SELECT proposal_id, fund_amount FROM disburse_proposals
                 WHERE guild_id = ?1 AND status = 'active'",
                [guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            transaction.commit()?;
            return Ok(false);
        };
        archive_votes(&transaction, guild_id, proposal_id, outcome)?;
        transaction.execute(
            "UPDATE disburse_proposals SET status = 'reset'
             WHERE guild_id = ?1 AND proposal_id = ?2 AND status = 'active'",
            params![guild_id, proposal_id],
        )?;
        transaction.execute(
            "DELETE FROM disburse_votes WHERE guild_id = ?1 AND proposal_id = ?2",
            params![guild_id, proposal_id],
        )?;
        if fund_amount > 0 {
            with_ledger_context(
                &transaction,
                proposal_id,
                "cancelled proposal funds returned to reserve",
                || {
                    let current = nonprofit_total(&transaction, guild_id)?;
                    let updated = current.checked_add(fund_amount).ok_or(
                        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                            "fund overflow",
                        ))),
                    )?;
                    set_nonprofit_total(&transaction, guild_id, updated)
                },
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn complete_reserve_allocation_atomic(
        &self,
        guild_id: Option<i64>,
        method: &str,
    ) -> Result<i64, DisbursementRepositoryError> {
        if !matches!(method, "burn" | "next_match_pot") {
            return Err(DisbursementRepositoryError::InvalidMethod(
                method.to_owned(),
            ));
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((proposal_id, amount)) = transaction
            .query_row(
                "SELECT proposal_id, fund_amount FROM disburse_proposals
                 WHERE guild_id = ?1 AND status = 'active'",
                [guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Err(DisbursementRepositoryError::NoActiveProposal);
        };
        archive_votes(&transaction, guild_id, proposal_id, method)?;
        transaction.execute(
            "UPDATE disburse_proposals SET status = 'completed'
             WHERE guild_id = ?1 AND proposal_id = ?2 AND status = 'active'",
            params![guild_id, proposal_id],
        )?;
        if method == "next_match_pot" {
            transaction.execute(
                "INSERT INTO nonprofit_fund(guild_id, total_collected, next_match_pot, updated_at)
                 VALUES (?1, 0, ?2, CURRENT_TIMESTAMP)
                 ON CONFLICT(guild_id) DO UPDATE SET
                    next_match_pot = COALESCE(nonprofit_fund.next_match_pot, 0) + excluded.next_match_pot,
                    updated_at = CURRENT_TIMESTAMP",
                params![guild_id, amount],
            )?;
        }
        transaction.execute(
            "INSERT INTO disburse_history
                 (guild_id, disbursed_at, total_amount, method, recipient_count, recipients)
             VALUES (?1, ?2, ?3, ?4, 0, '[]')",
            params![guild_id, unix_seconds(), amount, method],
        )?;
        transaction.commit()?;
        Ok(amount)
    }

    /// Complete the active proposal, return its locked reserve, debit the
    /// resulting payout, credit recipients, and write history atomically.
    pub fn complete_and_disburse_atomic(
        &self,
        guild_id: Option<i64>,
        method: &str,
        distributions: &[(i64, i64)],
    ) -> Result<i64, DisbursementRepositoryError> {
        validate_method(method)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let total = distributions.iter().try_fold(0_i64, |sum, (_, amount)| {
            sum.checked_add(*amount)
                .ok_or(DisbursementRepositoryError::AmountOverflow)
        })?;
        let recipients = distributions
            .iter()
            .map(|(discord_id, amount)| json!([discord_id, amount]))
            .collect::<Vec<_>>();
        let recipients_json = serde_json::to_string(&recipients)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((proposal_id, fund_amount)) = transaction
            .query_row(
                "SELECT proposal_id, fund_amount FROM disburse_proposals
                 WHERE guild_id = ?1 AND status = 'active'",
                [guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Err(DisbursementRepositoryError::NoActiveProposal);
        };
        archive_votes(&transaction, guild_id, proposal_id, method)?;
        transaction.execute(
            "UPDATE disburse_proposals SET status = 'completed'
             WHERE guild_id = ?1 AND proposal_id = ?2 AND status = 'active'",
            params![guild_id, proposal_id],
        )?;
        if fund_amount > 0 {
            with_ledger_context(
                &transaction,
                proposal_id,
                "Jopacoin Reserve proposal funds returned",
                || {
                    let current = nonprofit_total(&transaction, guild_id)?;
                    let updated = current.checked_add(fund_amount).ok_or(
                        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                            "fund overflow",
                        ))),
                    )?;
                    set_nonprofit_total(&transaction, guild_id, updated)
                },
            )?;
        }
        if total > 0 {
            let available = nonprofit_total(&transaction, guild_id)?;
            if available < total {
                return Err(DisbursementRepositoryError::InsufficientDistributionFunds {
                    available,
                    needed: total,
                });
            }
            with_ledger_context(
                &transaction,
                proposal_id,
                "Jopacoin Reserve disbursement debit",
                || set_nonprofit_total(&transaction, guild_id, available - total),
            )?;
            with_ledger_context(
                &transaction,
                proposal_id,
                "Jopacoin Reserve disbursement payout",
                || {
                    for (discord_id, amount) in distributions {
                        let changed = transaction.execute(
                            "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                             updated_at = CURRENT_TIMESTAMP
                             WHERE discord_id = ?2 AND guild_id = ?3",
                            params![amount, discord_id, guild_id],
                        )?;
                        if changed != 1 {
                            return Err(rusqlite::Error::QueryReturnedNoRows);
                        }
                    }
                    Ok(())
                },
            )?;
        }
        if !distributions.is_empty() {
            transaction.execute(
                "INSERT INTO disburse_history
                     (guild_id, disbursed_at, total_amount, method, recipient_count, recipients)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    guild_id,
                    unix_seconds(),
                    total,
                    method,
                    i64::try_from(distributions.len()).unwrap_or(i64::MAX),
                    recipients_json
                ],
            )?;
        }
        transaction.commit()?;
        Ok(total)
    }

    pub fn get_last_disbursement(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<DisbursementHistory>, DisbursementRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let Some(row) = connection
            .query_row(
                "SELECT id, guild_id, disbursed_at, total_amount, method,
                        recipient_count, recipients FROM disburse_history
                 WHERE guild_id = ?1 ORDER BY disbursed_at DESC, id DESC LIMIT 1",
                [guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let recipients = serde_json::from_str::<Vec<Vec<i64>>>(&row.6)?
            .into_iter()
            .filter_map(|pair| (pair.len() >= 2).then_some((pair[0], pair[1])))
            .collect();
        Ok(Some(DisbursementHistory {
            id: row.0,
            guild_id: row.1,
            disbursed_at: row.2,
            total_amount: row.3,
            method: row.4,
            recipient_count: row.5,
            recipients,
        }))
    }
}

fn zero_votes() -> BTreeMap<String, i64> {
    DISBURSE_METHODS
        .into_iter()
        .map(|method| (method.to_owned(), 0))
        .collect()
}

fn validate_method(method: &str) -> Result<(), DisbursementRepositoryError> {
    DISBURSE_METHODS
        .contains(&method)
        .then_some(())
        .ok_or_else(|| DisbursementRepositoryError::InvalidMethod(method.to_owned()))
}

fn nonprofit_total(transaction: &Transaction<'_>, guild_id: i64) -> Result<i64, rusqlite::Error> {
    Ok(transaction
        .query_row(
            "SELECT COALESCE(total_collected, 0) FROM nonprofit_fund WHERE guild_id = ?1",
            [guild_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn set_nonprofit_total(
    transaction: &Transaction<'_>,
    guild_id: i64,
    total: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO nonprofit_fund(guild_id, total_collected, updated_at)
         VALUES (?1, ?2, CURRENT_TIMESTAMP)
         ON CONFLICT(guild_id) DO UPDATE SET
             total_collected = excluded.total_collected,
             updated_at = CURRENT_TIMESTAMP",
        params![guild_id, total],
    )?;
    Ok(())
}

fn with_ledger_context<F, T>(
    transaction: &Transaction<'_>,
    proposal_id: i64,
    reason: &str,
    operation: F,
) -> Result<T, rusqlite::Error>
where
    F: FnOnce() -> Result<T, rusqlite::Error>,
{
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id, source, related_type, related_id, reason, metadata)
         VALUES (1, 'disburse', 'disbursement', ?1, ?2, ?3)",
        params![
            proposal_id.to_string(),
            reason,
            json!({"proposal_id": proposal_id}).to_string()
        ],
    )?;
    let result = operation();
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    result
}

fn archive_votes(
    transaction: &Transaction<'_>,
    guild_id: i64,
    proposal_id: i64,
    outcome: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT OR REPLACE INTO disburse_vote_history
             (guild_id, proposal_id, discord_id, vote_method, voted_at,
              proposal_outcome, finalized_at)
         SELECT guild_id, proposal_id, discord_id, vote_method, voted_at, ?1, ?2
         FROM disburse_votes WHERE guild_id = ?3 AND proposal_id = ?4",
        params![outcome, unix_seconds(), guild_id, proposal_id],
    )?;
    Ok(())
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_repositories::{NewPlayer, PlayerRepository};
    use crate::schema_manager::initialize_or_migrate;
    use tempfile::NamedTempFile;

    #[test]
    fn proposal_vote_reset_and_completion_are_durable_and_idempotent() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(1, "one", Some(7)))
            .expect("player");
        players
            .update_balance(1, Some(7), -20)
            .expect("debt balance");
        let loans = crate::loan_repository::LoanRepository::new(database.path());
        loans
            .add_to_nonprofit_fund(Some(7), 50, None)
            .expect("reserve");
        let repository = DisbursementRepository::new(database.path());
        let proposal = repository
            .create_proposal_atomic(Some(7), 100, 10, 1)
            .expect("proposal");
        assert_eq!(proposal.fund_amount, 50);
        let proposal = repository.add_vote(Some(7), 1, "even").expect("vote");
        assert!(proposal.quorum_reached());
        assert_eq!(
            repository
                .complete_and_disburse_atomic(Some(7), "even", &[(1, 20)])
                .expect("complete"),
            20
        );
        assert!(
            repository
                .get_active_proposal(Some(7))
                .expect("read proposal")
                .is_none()
        );
        assert_eq!(loans.get_nonprofit_fund(Some(7)).unwrap(), 30);
        assert_eq!(
            players
                .get_by_id(1, Some(7))
                .unwrap()
                .unwrap()
                .jopacoin_balance,
            0
        );
        assert!(matches!(
            repository.complete_and_disburse_atomic(Some(7), "even", &[(1, 1)]),
            Err(DisbursementRepositoryError::NoActiveProposal)
        ));
    }
}
