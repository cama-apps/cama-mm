//! Durable existing-schema Reserve proposal and ballot operations.
//!
//! Reserve locking, ballots, and settlement share a transaction boundary.
//! Settlement runs the domain allocation policy against the locked guild roster.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cama_domain::disbursement::{
    DistributionMethod, PlayerSnapshot, calculate_method_distributions,
};
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
pub struct DisbursementPayout {
    pub total: i64,
    pub distributions: Vec<(i64, i64)>,
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
    #[error("This proposal is no longer active.")]
    StaleProposal,
    #[error("The allocation ballot changed; please try again.")]
    BallotChanged,
    #[error("Quorum has not been reached.")]
    QuorumNotReached,
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
        read_active_proposal(&connection, guild_id)
    }

    /// The stored target tracks the last published quorum. Every decision uses the live one.
    pub fn proposals_needing_refresh(
        &self,
    ) -> Result<Vec<DisbursementProposal>, DisbursementRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT guild_id, quorum_required FROM disburse_proposals WHERE status='active'",
        )?;
        let rows =
            statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        let mut proposals = Vec::new();
        for row in rows {
            let (guild_id, published_quorum) = row?;
            if let Some(proposal) = read_active_proposal(&connection, guild_id)?
                && (proposal.quorum_required != published_quorum || proposal.quorum_reached())
            {
                proposals.push(proposal);
            }
        }
        Ok(proposals)
    }

    pub fn record_published_quorum(
        &self,
        proposal: &DisbursementProposal,
    ) -> Result<(), DisbursementRepositoryError> {
        self.connection()?.execute(
            "UPDATE disburse_proposals SET quorum_required=?1 WHERE guild_id=?2 AND proposal_id=?3 AND message_id IS ?4 AND status='active'",
            params![proposal.quorum_required, proposal.guild_id, proposal.proposal_id, proposal.message_id],
        )?;
        Ok(())
    }

    /// Lock the current Reserve amount and create one active proposal in the
    /// same transaction, using the guild's current match activity.
    pub fn create_proposal_atomic(
        &self,
        guild_id: Option<i64>,
        proposal_id: i64,
        minimum_fund: i64,
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
        let previous_id = transaction
            .query_row(
                "SELECT proposal_id FROM disburse_proposals WHERE guild_id=?1",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let proposal_id = match previous_id {
            Some(previous) => proposal_id.max(
                previous
                    .checked_add(1)
                    .ok_or(DisbursementRepositoryError::AmountOverflow)?,
            ),
            None => proposal_id,
        };
        let available = nonprofit_total(&transaction, guild_id)?;
        if available < minimum_fund {
            return Err(DisbursementRepositoryError::InsufficientFunds {
                available,
                minimum: minimum_fund,
            });
        }
        let quorum_required = active_quorum(&transaction, guild_id)?;
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
        // `guild_id` is the table primary key, so a completed or reset row still
        // occupies the slot. Drop it only after confirming there is no active vote.
        transaction.execute(
            "DELETE FROM disburse_proposals
             WHERE guild_id = ?1 AND status != 'active'",
            [guild_id],
        )?;
        transaction.execute(
            "INSERT INTO disburse_proposals
                 (guild_id, proposal_id, fund_amount, quorum_required, status)
             VALUES (?1, ?2, ?3, ?4, 'active')",
            params![guild_id, proposal_id, fund_amount, quorum_required],
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
            quorum_required,
            status: "active".to_owned(),
            created_at: None,
            votes: zero_votes(),
        })
    }

    pub fn set_proposal_message(
        &self,
        proposal: &DisbursementProposal,
        message_id: i64,
        channel_id: i64,
    ) -> Result<bool, DisbursementRepositoryError> {
        Ok(self.connection()?.execute(
            "UPDATE disburse_proposals SET message_id=?1, channel_id=?2, quorum_required=?3
             WHERE guild_id=?4 AND proposal_id=?5 AND status='active'",
            params![
                message_id,
                channel_id,
                proposal.quorum_required,
                proposal.guild_id,
                proposal.proposal_id
            ],
        )? == 1)
    }

    pub fn add_vote(
        &self,
        guild_id: Option<i64>,
        proposal_id: i64,
        discord_id: i64,
        method: &str,
    ) -> Result<DisbursementProposal, DisbursementRepositoryError> {
        validate_method(method)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active_id: i64 = transaction
            .query_row(
                "SELECT proposal_id FROM disburse_proposals WHERE guild_id=?1 AND status='active'",
                [guild_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DisbursementRepositoryError::NoActiveProposal)?;
        if active_id != proposal_id {
            return Err(DisbursementRepositoryError::StaleProposal);
        }
        let now = unix_seconds();
        transaction.execute(
            "INSERT INTO disburse_votes(guild_id, proposal_id, discord_id, vote_method, voted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(guild_id, proposal_id, discord_id) DO UPDATE SET
                 vote_method = excluded.vote_method, voted_at = excluded.voted_at",
            params![guild_id, proposal_id, discord_id, method, now],
        )?;
        let proposal = read_active_proposal(&transaction, guild_id)?
            .ok_or(DisbursementRepositoryError::NoActiveProposal)?;
        transaction.commit()?;
        Ok(proposal)
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
        self.reset_proposal_atomic(guild_id, outcome, None)
    }

    pub fn cancel_voted_proposal_atomic(
        &self,
        guild_id: Option<i64>,
        proposal_id: i64,
        require_quorum: bool,
    ) -> Result<bool, DisbursementRepositoryError> {
        self.reset_proposal_atomic(guild_id, "cancelled", Some((proposal_id, require_quorum)))
    }

    fn reset_proposal_atomic(
        &self,
        guild_id: Option<i64>,
        outcome: &str,
        expected: Option<(i64, bool)>,
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
        if let Some((expected_id, require_quorum)) = expected {
            validate_allocation(
                &transaction,
                guild_id,
                expected_id,
                "cancel",
                require_quorum,
            )?;
        }
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
        proposal_id: i64,
        method: &str,
        require_quorum: bool,
    ) -> Result<i64, DisbursementRepositoryError> {
        if !matches!(method, "burn" | "next_match_pot") {
            return Err(DisbursementRepositoryError::InvalidMethod(
                method.to_owned(),
            ));
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expected_proposal_id = proposal_id;
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
        validate_allocation(
            &transaction,
            guild_id,
            expected_proposal_id,
            method,
            require_quorum,
        )?;
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
        proposal_id: i64,
        method: &str,
        require_quorum: bool,
        lottery_cutoff: i64,
        draw: impl FnOnce(usize) -> usize,
    ) -> Result<DisbursementPayout, DisbursementRepositoryError> {
        let distribution_method = DistributionMethod::from_code(method)
            .filter(|method| {
                !matches!(
                    method,
                    DistributionMethod::Burn
                        | DistributionMethod::Cancel
                        | DistributionMethod::NextMatchPot
                )
            })
            .ok_or_else(|| DisbursementRepositoryError::InvalidMethod(method.to_owned()))?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let proposal =
            validate_allocation(&transaction, guild_id, proposal_id, method, require_quorum)?;
        let fund_amount = proposal.fund_amount;
        let roster = read_distribution_roster(&transaction, guild_id)?;
        let payouts = calculate_method_distributions(
            distribution_method,
            fund_amount,
            &roster,
            lottery_cutoff,
            draw,
        );
        let distributions: Vec<_> = payouts
            .into_iter()
            .map(|p| (p.discord_id, p.amount))
            .collect();
        let total = distributions.iter().try_fold(0_i64, |sum, (_, amount)| {
            sum.checked_add(*amount)
                .ok_or(DisbursementRepositoryError::AmountOverflow)
        })?;
        let recipients_json = serde_json::to_string(&distributions)?;
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
                    for (discord_id, amount) in &distributions {
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
        Ok(DisbursementPayout {
            total,
            distributions,
        })
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

fn read_distribution_roster(
    connection: &Connection,
    guild_id: i64,
) -> rusqlite::Result<Vec<PlayerSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT discord_id, COALESCE(discord_username,''), COALESCE(jopacoin_balance,0),
                COALESCE(wins,0), COALESCE(losses,0), unixepoch(last_match_date)
         FROM players WHERE guild_id=?1 AND discord_id IS NOT NULL ORDER BY discord_id",
    )?;
    statement
        .query_map([guild_id], |row| {
            Ok(PlayerSnapshot {
                discord_id: row.get(0)?,
                username: row.get(1)?,
                balance: row.get(2)?,
                wins: row.get::<_, i64>(3)?.clamp(0, i64::from(u32::MAX)) as u32,
                losses: row.get::<_, i64>(4)?.clamp(0, i64::from(u32::MAX)) as u32,
                last_match_at: row.get(5)?,
            })
        })?
        .collect()
}

/// Count each registered participant once, across recorded matches in the rolling window.
fn active_quorum(connection: &Connection, guild_id: i64) -> rusqlite::Result<i64> {
    let active: i64 = connection.query_row(
        "SELECT COUNT(DISTINCT mp.discord_id)
         FROM matches m
         CROSS JOIN match_participants mp ON mp.match_id=m.match_id AND mp.guild_id=m.guild_id
         JOIN players p ON p.discord_id=mp.discord_id AND p.guild_id=mp.guild_id
         WHERE m.guild_id=?1 AND mp.team_number IN (1,2)
           AND datetime(m.match_date, 'auto') BETWEEN datetime('now', '-30 days') AND datetime('now')",
        [guild_id],
        |row| row.get(0),
    )?;
    Ok(((active * 36 + 99) / 100).max(1))
}

fn read_active_proposal(
    connection: &Connection,
    guild_id: i64,
) -> Result<Option<DisbursementProposal>, DisbursementRepositoryError> {
    let Some(row) = connection
        .query_row(
            "SELECT guild_id, proposal_id, message_id, channel_id, fund_amount,
                        status, created_at
                 FROM disburse_proposals WHERE guild_id = ?1 AND status = 'active'",
            [guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
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
        quorum_required: active_quorum(connection, guild_id)?,
        status: row.5,
        created_at: row.6,
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

fn validate_allocation(
    connection: &Connection,
    guild_id: i64,
    proposal_id: i64,
    method: &str,
    require_quorum: bool,
) -> Result<DisbursementProposal, DisbursementRepositoryError> {
    let proposal = read_active_proposal(connection, guild_id)?
        .ok_or(DisbursementRepositoryError::NoActiveProposal)?;
    if proposal.proposal_id != proposal_id {
        return Err(DisbursementRepositoryError::StaleProposal);
    }
    if require_quorum && !proposal.quorum_reached() {
        return Err(DisbursementRepositoryError::QuorumNotReached);
    }
    let maximum = proposal.votes.values().copied().max().unwrap_or(0);
    let winner = DISBURSE_METHODS.into_iter().find(|candidate| {
        maximum > 0 && proposal.votes.get(*candidate).copied().unwrap_or(0) == maximum
    });
    if winner != Some(method) {
        return Err(DisbursementRepositoryError::BallotChanged);
    }
    Ok(proposal)
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
    use crate::test_support::copy_migrated_database;
    use tempfile::NamedTempFile;

    #[test]
    fn open_quorum_tracks_recent_match_participants_and_ignores_legacy_target() {
        let database = NamedTempFile::new().unwrap();
        copy_migrated_database(database.path()).unwrap();
        let repository = DisbursementRepository::new(database.path());
        let db = repository.connection().unwrap();
        db.execute_batch(
            "INSERT INTO disburse_proposals(guild_id,proposal_id,fund_amount,quorum_required,status)
             VALUES(7,100,50,99,'active');
             INSERT INTO players(guild_id,discord_id,discord_username)
             VALUES(7,1,'one'),(7,2,'two'),(7,3,'three'),(7,4,'four'),(7,5,'five'),(8,5,'five');
             INSERT INTO matches(match_id,guild_id,team1_players,team2_players,match_date)
             VALUES(1,7,'[]','[]',datetime('now','-1 day')),
                   (2,7,'[]','[]',unixepoch('now','-29 days')),
                   (3,7,'[]','[]',datetime('now','-31 days')),
                   (4,8,'[]','[]',CURRENT_TIMESTAMP);
             INSERT INTO match_participants(match_id,guild_id,discord_id,team_number)
             VALUES(1,7,1,1),(2,7,1,1),(2,7,2,1),(2,7,3,2),(2,7,99,2),
                   (3,7,4,2),(4,8,5,1);",
        ).unwrap();
        let ballot = repository.add_vote(Some(7), 100, 1, "burn").unwrap();
        assert_eq!(ballot.quorum_required, 2);
        assert!(!ballot.quorum_reached());
        db.execute(
            "UPDATE matches SET match_date=datetime('now','-31 days') WHERE match_id=2",
            [],
        )
        .unwrap();
        let ballot = repository.get_active_proposal(Some(7)).unwrap().unwrap();
        assert_eq!(ballot.quorum_required, 1);
        assert!(ballot.quorum_reached());
        db.execute(
            "UPDATE matches SET match_date=CURRENT_TIMESTAMP WHERE match_id=2",
            [],
        )
        .unwrap();
        assert!(matches!(
            repository.complete_reserve_allocation_atomic(Some(7), 100, "burn", true),
            Err(DisbursementRepositoryError::QuorumNotReached)
        ));
    }

    #[test]
    fn allocation_rejects_a_method_superseded_by_the_current_ballot() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = DisbursementRepository::new(database.path());
        crate::loan_repository::LoanRepository::new(database.path())
            .add_to_nonprofit_fund(Some(7), 50, None)
            .expect("reserve");
        repository.create_proposal_atomic(Some(7), 100, 10).unwrap();
        repository.add_vote(Some(7), 100, 1, "burn").unwrap();
        repository.add_vote(Some(7), 100, 1, "even").unwrap();

        assert!(
            repository
                .complete_reserve_allocation_atomic(Some(7), 100, "burn", true)
                .is_err()
        );
        assert_eq!(
            repository
                .get_active_proposal(Some(7))
                .unwrap()
                .unwrap()
                .proposal_id,
            100
        );
    }

    #[test]
    fn replacement_proposal_does_not_reuse_the_previous_identity() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = DisbursementRepository::new(database.path());
        repository.create_proposal_atomic(Some(7), 100, 0).unwrap();
        repository
            .reset_and_return_fund_atomic(Some(7), "reset")
            .unwrap();
        let replacement = repository.create_proposal_atomic(Some(7), 100, 0).unwrap();
        assert_ne!(replacement.proposal_id, 100);
    }

    #[test]
    fn stale_votes_and_allocations_cannot_mutate_a_replacement_proposal() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = DisbursementRepository::new(database.path());
        let loans = crate::loan_repository::LoanRepository::new(database.path());
        loans.add_to_nonprofit_fund(Some(7), 50, None).unwrap();
        repository.create_proposal_atomic(Some(7), 100, 10).unwrap();
        repository
            .reset_and_return_fund_atomic(Some(7), "reset")
            .unwrap();
        repository.create_proposal_atomic(Some(7), 100, 10).unwrap();
        let replacement = repository.get_active_proposal(Some(7)).unwrap().unwrap();

        assert!(matches!(
            repository.add_vote(Some(7), 100, 1, "burn"),
            Err(DisbursementRepositoryError::StaleProposal)
        ));
        for method in ["burn", "next_match_pot"] {
            assert!(matches!(
                repository.complete_reserve_allocation_atomic(Some(7), 100, method, false),
                Err(DisbursementRepositoryError::StaleProposal)
            ));
        }
        assert!(matches!(
            repository.complete_and_disburse_atomic(
                Some(7),
                100,
                "even",
                false,
                0,
                |_| unreachable!()
            ),
            Err(DisbursementRepositoryError::StaleProposal)
        ));
        assert!(matches!(
            repository.cancel_voted_proposal_atomic(Some(7), 100, false),
            Err(DisbursementRepositoryError::StaleProposal)
        ));
        assert_eq!(
            repository.get_active_proposal(Some(7)).unwrap().unwrap(),
            replacement
        );
        assert_eq!(loans.get_nonprofit_fund(Some(7)).unwrap(), 0);
        assert!(repository.get_last_disbursement(Some(7)).unwrap().is_none());
    }

    #[test]
    fn automatic_allocation_requires_quorum_but_admin_execution_can_override_it() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = DisbursementRepository::new(database.path());
        repository.connection().unwrap().execute_batch(
            "INSERT INTO players(guild_id,discord_id,discord_username) VALUES(7,1,'a'),(7,2,'b'),(7,3,'c');
             INSERT INTO matches(match_id,guild_id,team1_players,team2_players) VALUES(1,7,'[]','[]');
             INSERT INTO match_participants(match_id,guild_id,discord_id,team_number) VALUES(1,7,1,1),(1,7,2,1),(1,7,3,2);"
        ).unwrap();
        repository.create_proposal_atomic(Some(7), 100, 0).unwrap();
        repository.add_vote(Some(7), 100, 1, "burn").unwrap();
        assert!(matches!(
            repository.complete_reserve_allocation_atomic(Some(7), 100, "burn", true),
            Err(DisbursementRepositoryError::QuorumNotReached)
        ));
        repository
            .complete_reserve_allocation_atomic(Some(7), 100, "burn", false)
            .unwrap();
        assert!(repository.get_active_proposal(Some(7)).unwrap().is_none());
    }

    #[test]
    fn stale_message_publication_cannot_replace_the_new_proposals_message() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let repository = DisbursementRepository::new(database.path());
        let old = repository.create_proposal_atomic(Some(7), 100, 0).unwrap();
        repository
            .reset_and_return_fund_atomic(Some(7), "reset")
            .unwrap();
        let new = repository.create_proposal_atomic(Some(7), 200, 0).unwrap();
        assert!(repository.set_proposal_message(&new, 22, 33).unwrap());
        assert!(!repository.set_proposal_message(&old, 44, 55).unwrap());
        let proposal = repository.get_active_proposal(Some(7)).unwrap().unwrap();
        assert_eq!(proposal.proposal_id, 200);
        assert_eq!(proposal.message_id, Some(22));
        assert_eq!(proposal.channel_id, Some(33));
    }

    #[test]
    fn proposal_vote_reset_and_completion_are_durable_and_idempotent() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
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
            .create_proposal_atomic(Some(7), 100, 10)
            .expect("proposal");
        assert_eq!(proposal.fund_amount, 50);
        let proposal = repository.add_vote(Some(7), 100, 1, "even").expect("vote");
        assert!(proposal.quorum_reached());
        assert_eq!(
            repository
                .complete_and_disburse_atomic(Some(7), 100, "even", true, 0, |_| unreachable!())
                .expect("complete")
                .total,
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
            repository.complete_and_disburse_atomic(
                Some(7),
                100,
                "even",
                true,
                0,
                |_| unreachable!()
            ),
            Err(DisbursementRepositoryError::NoActiveProposal)
        ));
    }

    #[test]
    fn settlement_reads_lottery_eligibility_from_the_current_guild_roster() {
        let database = NamedTempFile::new().unwrap();
        copy_migrated_database(database.path()).unwrap();
        let repository = DisbursementRepository::new(database.path());
        repository
            .connection()
            .unwrap()
            .execute_batch(
                "INSERT INTO players(guild_id,discord_id,discord_username,last_match_date)
             VALUES(7,1,'old','2025-01-01'),(7,2,'rfc3339','2026-01-02T12:00:00+00:00'),
                   (7,3,'none',NULL),(7,4,'sqlite','2026-01-02 12:00:00'),
                   (8,5,'other','2026-01-02 12:00:00');",
            )
            .unwrap();
        let loans = crate::loan_repository::LoanRepository::new(database.path());
        for (proposal_id, index, winner) in [(100, 0, 2), (101, 1, 4)] {
            loans.add_to_nonprofit_fund(Some(7), 250, None).unwrap();
            repository
                .create_proposal_atomic(Some(7), proposal_id, 250)
                .unwrap();
            repository
                .add_vote(Some(7), proposal_id, 2, "lottery")
                .unwrap();
            let cutoff = 1_767_225_600; // 2026-01-01 UTC
            let payout = repository
                .complete_and_disburse_atomic(
                    Some(7),
                    proposal_id,
                    "lottery",
                    true,
                    cutoff,
                    |len| {
                        assert_eq!(len, 2);
                        index
                    },
                )
                .unwrap();
            assert_eq!(payout.distributions, vec![(winner, 250)]);
        }
    }

    #[test]
    fn settlement_uses_current_guild_balances_wins_and_losses() {
        let database = NamedTempFile::new().unwrap();
        copy_migrated_database(database.path()).unwrap();
        let repository = DisbursementRepository::new(database.path());
        repository.connection().unwrap().execute_batch(
            "INSERT INTO players(guild_id,discord_id,discord_username,jopacoin_balance,wins,losses)
             VALUES(7,1,'first',90,2,0),(7,2,'second',80,0,2),(7,3,'third',70,1,1),
                   (7,4,'fourth',10,3,3),(7,5,'fifth',-50,0,4),(8,6,'other',999,99,99);"
        ).unwrap();
        let loans = crate::loan_repository::LoanRepository::new(database.path());
        loans.add_to_nonprofit_fund(Some(7), 100, None).unwrap();
        repository
            .create_proposal_atomic(Some(7), 100, 100)
            .unwrap();
        repository
            .add_vote(Some(7), 100, 4, "social_security")
            .unwrap();
        let payout = repository
            .complete_and_disburse_atomic(
                Some(7),
                100,
                "social_security",
                true,
                0,
                |_| unreachable!(),
            )
            .unwrap();
        assert_eq!(payout.distributions, vec![(4, 60), (5, 40)]);
    }

    #[test]
    fn settlement_uses_balance_after_the_ballot_was_cast() {
        let database = NamedTempFile::new().unwrap();
        copy_migrated_database(database.path()).unwrap();
        let repository = DisbursementRepository::new(database.path());
        let players = PlayerRepository::new(database.path());
        players.add(&NewPlayer::new(1, "one", Some(7))).unwrap();
        players.update_balance(1, Some(7), -100).unwrap();
        let loans = crate::loan_repository::LoanRepository::new(database.path());
        loans.add_to_nonprofit_fund(Some(7), 250, None).unwrap();
        repository
            .create_proposal_atomic(Some(7), 100, 250)
            .unwrap();
        repository.add_vote(Some(7), 100, 1, "even").unwrap();
        players.update_balance(1, Some(7), 0).unwrap();
        let payout = repository
            .complete_and_disburse_atomic(Some(7), 100, "even", true, 0, |_| unreachable!())
            .unwrap();
        assert!(payout.distributions.is_empty());
        assert_eq!(loans.get_nonprofit_fund(Some(7)).unwrap(), 250);
        assert_eq!(
            players
                .get_by_id(1, Some(7))
                .unwrap()
                .unwrap()
                .jopacoin_balance,
            0
        );
    }

    #[test]
    fn create_proposal_succeeds_after_completed_row_still_occupies_guild() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
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
        repository
            .create_proposal_atomic(Some(7), 100, 10)
            .expect("proposal");
        repository.add_vote(Some(7), 100, 1, "even").expect("vote");
        repository
            .complete_and_disburse_atomic(Some(7), 100, "even", true, 0, |_| unreachable!())
            .expect("complete");
        assert!(
            repository
                .get_active_proposal(Some(7))
                .expect("read proposal")
                .is_none()
        );

        let proposal = repository
            .create_proposal_atomic(Some(7), 200, 10)
            .expect("second proposal");
        assert_eq!(proposal.proposal_id, 200);
        assert_eq!(proposal.fund_amount, 30);
        assert_eq!(proposal.status, "active");
        assert_eq!(loans.get_nonprofit_fund(Some(7)).unwrap(), 0);
        assert_eq!(
            repository
                .get_active_proposal(Some(7))
                .expect("read proposal")
                .expect("active")
                .proposal_id,
            200
        );
    }

    #[test]
    fn create_proposal_succeeds_after_reset_row_still_occupies_guild() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let loans = crate::loan_repository::LoanRepository::new(database.path());
        loans
            .add_to_nonprofit_fund(Some(7), 50, None)
            .expect("reserve");
        let repository = DisbursementRepository::new(database.path());
        repository
            .create_proposal_atomic(Some(7), 100, 10)
            .expect("proposal");
        assert!(
            repository
                .reset_and_return_fund_atomic(Some(7), "cancel")
                .expect("reset")
        );
        assert!(
            repository
                .get_active_proposal(Some(7))
                .expect("read proposal")
                .is_none()
        );

        let proposal = repository
            .create_proposal_atomic(Some(7), 200, 10)
            .expect("second proposal");
        assert_eq!(proposal.proposal_id, 200);
        assert_eq!(proposal.fund_amount, 50);
        assert_eq!(loans.get_nonprofit_fund(Some(7)).unwrap(), 0);
    }

    #[test]
    fn create_proposal_still_rejects_an_active_vote() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("schema");
        let loans = crate::loan_repository::LoanRepository::new(database.path());
        loans
            .add_to_nonprofit_fund(Some(7), 50, None)
            .expect("reserve");
        let repository = DisbursementRepository::new(database.path());
        repository
            .create_proposal_atomic(Some(7), 100, 10)
            .expect("proposal");
        assert!(matches!(
            repository.create_proposal_atomic(Some(7), 200, 10),
            Err(DisbursementRepositoryError::ActiveProposal)
        ));
        assert_eq!(
            repository
                .get_active_proposal(Some(7))
                .expect("read proposal")
                .expect("active")
                .proposal_id,
            100
        );
        assert_eq!(loans.get_nonprofit_fund(Some(7)).unwrap(), 0);
    }
}
