//! Python-compatible loan and nonprofit-fund persistence.
//!
//! Python remains the production runtime and the only schema-migration
//! authority during the side-by-side rewrite. This repository only opens an
//! existing SQLite database through [`crate::open_runtime_connection`]; no
//! production path creates, alters, or migrates schema. Mutating operations
//! that span balances, loan state, the nonprofit reserve, or trigger-generated
//! ledger rows use `BEGIN IMMEDIATE`, matching Python's five-second lock and
//! transaction contract.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

const SQLITE_ID_CHUNK: usize = 900;

/// One persisted row from Python's guild-scoped `loan_state` table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoanStateRecord {
    pub discord_id: i64,
    pub guild_id: i64,
    pub last_loan_at: Option<i64>,
    pub total_loans_taken: i64,
    pub total_fees_paid: i64,
    pub negative_loans_taken: i64,
    pub outstanding_principal: i64,
    pub outstanding_fee: i64,
}

impl LoanStateRecord {
    #[must_use]
    pub fn has_outstanding_loan(self) -> bool {
        self.outstanding_principal > 0
    }

    #[must_use]
    pub fn outstanding_total(self) -> Option<i64> {
        self.outstanding_principal.checked_add(self.outstanding_fee)
    }
}

/// Nullable patch semantics used by Python's `upsert_state` helper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoanStatePatch {
    pub last_loan_at: Option<i64>,
    pub total_loans_taken: Option<i64>,
    pub total_fees_paid: Option<i64>,
    pub negative_loans_taken: Option<i64>,
    pub outstanding_principal: Option<i64>,
    pub outstanding_fee: Option<i64>,
}

/// Context consumed by Python-owned economy-ledger triggers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LedgerContext {
    pub source: Option<String>,
    pub actor_id: Option<i64>,
    pub related_type: Option<String>,
    pub related_id: Option<String>,
    pub reason: Option<String>,
    pub metadata: Option<String>,
}

impl LedgerContext {
    fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.actor_id.is_none()
            && self.related_type.is_none()
            && self.related_id.is_none()
            && self.reason.is_none()
            && self.metadata.is_none()
    }
}

/// Successful atomic loan issuance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoanExecution {
    pub amount: i64,
    pub fee: i64,
    pub total_owed: i64,
    pub new_balance: i64,
    pub total_loans_taken: i64,
    pub was_negative_loan: bool,
}

/// Successful atomic loan settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoanRepayment {
    pub principal: i64,
    pub fee: i64,
    pub total_owed: i64,
    pub balance_before: i64,
    pub new_balance: i64,
    pub nonprofit_total: i64,
}

/// One atomic payment made by a player toward another player's negative
/// balance. This mirrors `PlayerRepository.pay_debt_atomic`: the requested
/// amount is capped at the recipient's debt, and both wallet updates plus
/// their ledger context commit together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebtPayment {
    pub amount_paid: i64,
    pub from_new_balance: i64,
    pub to_new_balance: i64,
}

/// One ordered stipend result. Duplicate input IDs are represented once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StipendPayment {
    pub discord_id: i64,
    pub amount: i64,
}

#[derive(Debug, Error)]
pub enum LoanRepositoryError {
    #[error(
        "You have an outstanding loan of {total_owed} (principal: {principal}, fee: {fee}). Repay it by playing in a match first!"
    )]
    OutstandingLoan {
        principal: i64,
        fee: i64,
        total_owed: i64,
    },
    #[error("Loan cooldown active. Try again in {hours}h {minutes}m.")]
    CooldownActive { hours: i64, minutes: i64 },
    #[error("Loan amount must be positive.")]
    InvalidLoanAmount,
    #[error("Maximum loan amount is {max_amount}.")]
    LoanAmountExceeded { max_amount: i64 },
    #[error("Player not found.")]
    PlayerNotFound,
    #[error("No outstanding loan to repay.")]
    NoOutstandingLoan,
    #[error("Insufficient balance. You have {0} jopacoin.")]
    InsufficientDebtPaymentBalance(i64),
    #[error("Recipient has no debt to pay off.")]
    RecipientHasNoDebt,
    #[error("Amount must be positive")]
    InvalidFundAmount,
    #[error("Insufficient nonprofit funds. Available: {available}, requested: {requested}")]
    InsufficientNonprofitFunds { available: i64, requested: i64 },
    #[error("nonprofit stipend reserve changed unexpectedly")]
    StipendReserveChanged,
    #[error("stipend recipient changed unexpectedly")]
    StipendRecipientChanged,
    #[error("loan monetary arithmetic exceeded SQLite's signed 64-bit range")]
    AmountOverflow,
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("loan SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Existing-schema repository for loans and their nonprofit reserve effects.
#[derive(Clone, Debug)]
pub struct LoanRepository {
    path: PathBuf,
}

impl LoanRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Convert Python's nullable-guild convention to its persisted sentinel.
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
    ) -> Result<Option<LoanStateRecord>, LoanRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT discord_id, guild_id, last_loan_at,
                        COALESCE(total_loans_taken, 0),
                        COALESCE(total_fees_paid, 0),
                        COALESCE(negative_loans_taken, 0),
                        COALESCE(outstanding_principal, 0),
                        COALESCE(outstanding_fee, 0)
                 FROM loan_state
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| {
                    Ok(LoanStateRecord {
                        discord_id: row.get(0)?,
                        guild_id: row.get(1)?,
                        last_loan_at: row.get(2)?,
                        total_loans_taken: row.get(3)?,
                        total_fees_paid: row.get(4)?,
                        negative_loans_taken: row.get(5)?,
                        outstanding_principal: row.get(6)?,
                        outstanding_fee: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Return requested IDs with positive principal, in one connection and at
    /// most one query for ordinary match-sized input. Duplicates are removed
    /// before binding and guild scope is part of every query.
    pub fn get_outstanding_borrower_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashSet<i64>, LoanRepositoryError> {
        let unique_ids = unique_in_input_order(discord_ids);
        if unique_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut outstanding = HashSet::new();
        for chunk in unique_ids.chunks(SQLITE_ID_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT discord_id
                 FROM loan_state
                 WHERE guild_id = ?
                   AND discord_id IN ({placeholders})
                   AND COALESCE(outstanding_principal, 0) > 0"
            );
            let bindings = std::iter::once(guild_id).chain(chunk.iter().copied());
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(bindings), |row| row.get(0))?;
            outstanding.extend(rows.collect::<Result<Vec<i64>, _>>()?);
        }
        Ok(outstanding)
    }

    pub fn upsert_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        patch: LoanStatePatch,
    ) -> Result<(), LoanRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO loan_state (
                 discord_id, guild_id, last_loan_at, total_loans_taken,
                 total_fees_paid, negative_loans_taken,
                 outstanding_principal, outstanding_fee, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 last_loan_at = COALESCE(excluded.last_loan_at, loan_state.last_loan_at),
                 total_loans_taken = COALESCE(
                     excluded.total_loans_taken, loan_state.total_loans_taken
                 ),
                 total_fees_paid = COALESCE(
                     excluded.total_fees_paid, loan_state.total_fees_paid
                 ),
                 negative_loans_taken = COALESCE(
                     excluded.negative_loans_taken, loan_state.negative_loans_taken
                 ),
                 outstanding_principal = COALESCE(
                     excluded.outstanding_principal, loan_state.outstanding_principal
                 ),
                 outstanding_fee = COALESCE(
                     excluded.outstanding_fee, loan_state.outstanding_fee
                 ),
                 updated_at = CURRENT_TIMESTAMP",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                patch.last_loan_at,
                patch.total_loans_taken,
                patch.total_fees_paid,
                patch.negative_loans_taken,
                patch.outstanding_principal,
                patch.outstanding_fee,
            ],
        )?;
        Ok(())
    }

    /// Clear only the cooldown column. A missing row is deliberately a no-op.
    pub fn reset_cooldown(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), LoanRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE loan_state
             SET last_loan_at = 0, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(())
    }

    pub fn get_player_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, LoanRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0)
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_nonprofit_fund(&self, guild_id: Option<i64>) -> Result<i64, LoanRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT COALESCE(total_collected, 0)
                 FROM nonprofit_fund WHERE guild_id = ?1",
                params![Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Credit the reserve and return the exact post-call balance while holding
    /// the immediate write lock.
    pub fn add_to_nonprofit_fund(
        &self,
        guild_id: Option<i64>,
        amount: i64,
        context: Option<&LedgerContext>,
    ) -> Result<i64, LoanRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = nonprofit_total(&transaction, guild_id)?;
        let updated = current
            .checked_add(amount)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        with_optional_ledger_context(&transaction, context, || {
            set_nonprofit_total(&transaction, guild_id, updated)
        })?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Debit one existing player and credit the reserve in one transaction.
    pub fn transfer_balance_to_nonprofit_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        context: &LedgerContext,
    ) -> Result<i64, LoanRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balance = player_balance(&transaction, discord_id, guild_id)?
            .ok_or(LoanRepositoryError::PlayerNotFound)?;
        let new_balance = balance
            .checked_sub(amount)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let current_fund = nonprofit_total(&transaction, guild_id)?;
        let new_fund = current_fund
            .checked_add(amount)
            .ok_or(LoanRepositoryError::AmountOverflow)?;

        with_ledger_context(&transaction, context, || {
            let changed = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![new_balance, discord_id, guild_id],
            )?;
            if changed != 1 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            set_nonprofit_total(&transaction, guild_id, new_fund)
        })?;
        transaction.commit()?;
        Ok(new_fund)
    }

    /// Atomically transfer existing balance to a player with a negative
    /// balance, capping the payment at the recipient's debt. This is kept in
    /// the existing-schema loan/economy repository so `/economy paydebt` does
    /// not need a second transaction or a runtime-local balance model.
    pub fn pay_debt_atomic(
        &self,
        from_discord_id: i64,
        to_discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<DebtPayment, LoanRepositoryError> {
        if amount <= 0 {
            return Err(LoanRepositoryError::InvalidFundAmount);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let from_balance = player_balance(&transaction, from_discord_id, guild_id)?
            .ok_or(LoanRepositoryError::PlayerNotFound)?;
        if from_balance < amount {
            return Err(LoanRepositoryError::InsufficientDebtPaymentBalance(
                from_balance,
            ));
        }
        let to_balance = player_balance(&transaction, to_discord_id, guild_id)?
            .ok_or(LoanRepositoryError::PlayerNotFound)?;
        if to_balance >= 0 {
            return Err(LoanRepositoryError::RecipientHasNoDebt);
        }
        let amount_paid = amount.min(to_balance.saturating_abs());
        let from_new_balance = from_balance
            .checked_sub(amount_paid)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let to_new_balance = to_balance
            .checked_add(amount_paid)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let metadata = format!(r#"{{"amount_paid": {amount_paid}}}"#);
        let from_context = LedgerContext {
            source: Some("debt_payment".to_owned()),
            actor_id: Some(from_discord_id),
            related_type: Some("player_debt".to_owned()),
            related_id: Some(to_discord_id.to_string()),
            reason: Some("debt payment sender debit".to_owned()),
            metadata: Some(metadata.clone()),
        };
        with_ledger_context(&transaction, &from_context, || {
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![from_new_balance, from_discord_id, guild_id],
            )?;
            if changed == 1 {
                Ok(())
            } else {
                Err(rusqlite::Error::QueryReturnedNoRows)
            }
        })?;
        let to_context = LedgerContext {
            source: Some("debt_payment".to_owned()),
            actor_id: Some(from_discord_id),
            related_type: Some("player_debt".to_owned()),
            related_id: Some(to_discord_id.to_string()),
            reason: Some("debt payment recipient credit".to_owned()),
            metadata: Some(metadata),
        };
        with_ledger_context(&transaction, &to_context, || {
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![to_new_balance, to_discord_id, guild_id],
            )?;
            if changed == 1 {
                Ok(())
            } else {
                Err(rusqlite::Error::QueryReturnedNoRows)
            }
        })?;
        transaction.commit()?;
        Ok(DebtPayment {
            amount_paid,
            from_new_balance,
            to_new_balance,
        })
    }

    /// Deduct a positive amount from the reserve under an immediate lock.
    pub fn deduct_from_nonprofit_fund(
        &self,
        guild_id: Option<i64>,
        amount: i64,
        context: Option<&LedgerContext>,
    ) -> Result<i64, LoanRepositoryError> {
        if amount <= 0 {
            return Err(LoanRepositoryError::InvalidFundAmount);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = nonprofit_total(&transaction, guild_id)?;
        if current < amount {
            return Err(LoanRepositoryError::InsufficientNonprofitFunds {
                available: current,
                requested: amount,
            });
        }
        let updated = current - amount;
        with_optional_ledger_context(&transaction, context, || {
            set_nonprofit_total(&transaction, guild_id, updated)
        })?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Atomically reset the guild's active disbursement proposal and return
    /// its reserved funds. A retry observes no active proposal and is a no-op,
    /// so the reserve cannot be credited twice.
    pub fn reset_and_return_fund_atomic(
        &self,
        guild_id: Option<i64>,
        fund_amount_to_return: i64,
    ) -> Result<bool, LoanRepositoryError> {
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
            .optional()?;
        let Some(proposal_id) = proposal_id else {
            transaction.commit()?;
            return Ok(false);
        };

        let changed = transaction.execute(
            "UPDATE disburse_proposals SET status = 'reset'
             WHERE guild_id = ?1 AND proposal_id = ?2 AND status = 'active'",
            params![guild_id, proposal_id],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "DELETE FROM disburse_votes WHERE guild_id = ?1 AND proposal_id = ?2",
            params![guild_id, proposal_id],
        )?;
        if fund_amount_to_return != 0 {
            let updated = nonprofit_total(&transaction, guild_id)?
                .checked_add(fund_amount_to_return)
                .ok_or(LoanRepositoryError::AmountOverflow)?;
            let context = LedgerContext {
                source: Some("disburse".to_owned()),
                related_type: Some("disbursement".to_owned()),
                related_id: Some(proposal_id.to_string()),
                reason: Some("cancelled proposal funds returned to reserve".to_owned()),
                metadata: Some(format!(r#"{{"fund_amount": {fund_amount_to_return}}}"#)),
                actor_id: None,
            };
            with_ledger_context(&transaction, &context, || {
                set_nonprofit_total(&transaction, guild_id, updated)
            })?;
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Pay unique, ordered bankrupt recipients until the reserve is empty.
    pub fn distribute_nonprofit_stipends_atomic(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        max_stipend: i64,
    ) -> Result<Vec<StipendPayment>, LoanRepositoryError> {
        let unique_ids = unique_in_input_order(discord_ids);
        let mut paid = unique_ids
            .iter()
            .map(|discord_id| StipendPayment {
                discord_id: *discord_id,
                amount: 0,
            })
            .collect::<Vec<_>>();
        if unique_ids.is_empty() || max_stipend <= 0 {
            return Ok(paid);
        }

        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut remaining = nonprofit_total(&transaction, guild_id)?.max(0);
        if remaining == 0 {
            transaction.commit()?;
            return Ok(paid);
        }

        let eligible = eligible_bankrupt_ids(&transaction, &unique_ids, guild_id)?;
        for payment in &mut paid {
            if remaining == 0 || !eligible.contains(&payment.discord_id) {
                continue;
            }
            let amount = max_stipend.min(remaining);
            let reserve_context = LedgerContext {
                source: Some("mana".to_owned()),
                related_type: Some("bankruptcy_stipend".to_owned()),
                related_id: Some(payment.discord_id.to_string()),
                reason: Some("white mana bankruptcy stipend reserve debit".to_owned()),
                metadata: Some(format!(r#"{{"amount": {amount}, "land": "Plains"}}"#)),
                actor_id: None,
            };
            with_ledger_context(&transaction, &reserve_context, || {
                let changed = transaction.execute(
                    "UPDATE nonprofit_fund
                     SET total_collected = total_collected - ?1,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE guild_id = ?2 AND total_collected >= ?1",
                    params![amount, guild_id],
                )?;
                if changed == 1 {
                    Ok(())
                } else {
                    Err(rusqlite::Error::QueryReturnedNoRows)
                }
            })
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => LoanRepositoryError::StipendReserveChanged,
                other => LoanRepositoryError::Sqlite(other),
            })?;

            let recipient_context = LedgerContext {
                source: Some("mana".to_owned()),
                related_type: Some("bankruptcy_stipend".to_owned()),
                reason: Some("white mana bankruptcy stipend".to_owned()),
                metadata: Some(format!(r#"{{"amount": {amount}, "land": "Plains"}}"#)),
                ..LedgerContext::default()
            };
            with_ledger_context(&transaction, &recipient_context, || {
                let changed = transaction.execute(
                    "UPDATE players
                     SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?2 AND guild_id = ?3
                       AND COALESCE(jopacoin_balance, 0) <= 0",
                    params![amount, payment.discord_id, guild_id],
                )?;
                if changed == 1 {
                    Ok(())
                } else {
                    Err(rusqlite::Error::QueryReturnedNoRows)
                }
            })
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    LoanRepositoryError::StipendRecipientChanged
                }
                other => LoanRepositoryError::Sqlite(other),
            })?;
            payment.amount = amount;
            remaining -= amount;
        }
        transaction.commit()?;
        Ok(paid)
    }

    /// Execute with the system clock, matching the public Python repository.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_loan_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        fee: i64,
        cooldown_seconds: i64,
        max_amount: i64,
    ) -> Result<LoanExecution, LoanRepositoryError> {
        self.execute_loan_atomic_at(
            discord_id,
            guild_id,
            amount,
            fee,
            cooldown_seconds,
            max_amount,
            unix_timestamp()?,
        )
    }

    /// Deterministic-clock form used by typed application adapters and tests.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_loan_atomic_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        fee: i64,
        cooldown_seconds: i64,
        max_amount: i64,
        now: i64,
    ) -> Result<LoanExecution, LoanRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let state = loan_state_in_transaction(&transaction, discord_id, guild_id)?;
        if let Some(state) = state {
            if state.outstanding_principal > 0 {
                let total_owed = state
                    .outstanding_total()
                    .ok_or(LoanRepositoryError::AmountOverflow)?;
                return Err(LoanRepositoryError::OutstandingLoan {
                    principal: state.outstanding_principal,
                    fee: state.outstanding_fee,
                    total_owed,
                });
            }
            if let Some(last_loan_at) = state.last_loan_at.filter(|last| *last != 0) {
                let elapsed = i128::from(now) - i128::from(last_loan_at);
                if elapsed < i128::from(cooldown_seconds) {
                    let remaining = (i128::from(cooldown_seconds) - elapsed)
                        .clamp(0, i128::from(i64::MAX)) as i64;
                    return Err(LoanRepositoryError::CooldownActive {
                        hours: remaining / 3_600,
                        minutes: (remaining % 3_600) / 60,
                    });
                }
            }
        }
        if amount <= 0 {
            return Err(LoanRepositoryError::InvalidLoanAmount);
        }
        if amount > max_amount {
            return Err(LoanRepositoryError::LoanAmountExceeded { max_amount });
        }

        let balance_before = player_balance(&transaction, discord_id, guild_id)?
            .ok_or(LoanRepositoryError::PlayerNotFound)?;
        let new_balance = balance_before
            .checked_add(amount)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let total_owed = amount
            .checked_add(fee)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let state = state.unwrap_or(LoanStateRecord {
            discord_id,
            guild_id,
            ..LoanStateRecord::default()
        });
        let total_loans_taken = state
            .total_loans_taken
            .checked_add(1)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let was_negative_loan = balance_before < 0;
        let negative_loans_taken = state
            .negative_loans_taken
            .checked_add(i64::from(was_negative_loan))
            .ok_or(LoanRepositoryError::AmountOverflow)?;

        let context = LedgerContext {
            source: Some("loan".to_owned()),
            related_type: Some("loan".to_owned()),
            related_id: Some(discord_id.to_string()),
            reason: Some("loan principal credit".to_owned()),
            metadata: Some(format!(r#"{{"amount": {amount}, "fee": {fee}}}"#)),
            actor_id: None,
        };
        with_ledger_context(&transaction, &context, || {
            transaction.execute(
                "UPDATE players
                 SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![new_balance, discord_id, guild_id],
            )?;
            Ok(())
        })?;
        transaction.execute(
            "INSERT INTO loan_state (
                 discord_id, guild_id, last_loan_at, total_loans_taken,
                 total_fees_paid, negative_loans_taken,
                 outstanding_principal, outstanding_fee, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 last_loan_at = excluded.last_loan_at,
                 total_loans_taken = excluded.total_loans_taken,
                 negative_loans_taken = excluded.negative_loans_taken,
                 outstanding_principal = excluded.outstanding_principal,
                 outstanding_fee = excluded.outstanding_fee,
                 updated_at = CURRENT_TIMESTAMP",
            params![
                discord_id,
                guild_id,
                now,
                total_loans_taken,
                state.total_fees_paid,
                negative_loans_taken,
                amount,
                fee,
            ],
        )?;
        transaction.commit()?;

        Ok(LoanExecution {
            amount,
            fee,
            total_owed,
            new_balance,
            total_loans_taken,
            was_negative_loan,
        })
    }

    pub fn execute_repayment_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<LoanRepayment, LoanRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = loan_state_in_transaction(&transaction, discord_id, guild_id)?
            .ok_or(LoanRepositoryError::NoOutstandingLoan)?;
        let total_owed = state
            .outstanding_principal
            .checked_add(state.outstanding_fee)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        if total_owed <= 0 {
            return Err(LoanRepositoryError::NoOutstandingLoan);
        }
        let balance_before = player_balance(&transaction, discord_id, guild_id)?
            .ok_or(LoanRepositoryError::PlayerNotFound)?;
        let new_balance = balance_before
            .checked_sub(total_owed)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let current_fund = nonprofit_total(&transaction, guild_id)?;
        let nonprofit_total = current_fund
            .checked_add(state.outstanding_fee)
            .ok_or(LoanRepositoryError::AmountOverflow)?;
        let total_fees_paid = state
            .total_fees_paid
            .checked_add(state.outstanding_fee)
            .ok_or(LoanRepositoryError::AmountOverflow)?;

        let repayment_context = LedgerContext {
            source: Some("loan_repayment".to_owned()),
            related_type: Some("loan".to_owned()),
            related_id: Some(discord_id.to_string()),
            reason: Some("loan principal and fee repayment".to_owned()),
            metadata: Some(format!(
                r#"{{"principal": {}, "fee": {}, "total_owed": {total_owed}}}"#,
                state.outstanding_principal, state.outstanding_fee
            )),
            actor_id: None,
        };
        with_ledger_context(&transaction, &repayment_context, || {
            transaction.execute(
                "UPDATE players
                 SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![new_balance, discord_id, guild_id],
            )?;
            Ok(())
        })?;

        let reserve_context = LedgerContext {
            reason: Some("loan repayment fee reserve credit".to_owned()),
            ..repayment_context
        };
        with_ledger_context(&transaction, &reserve_context, || {
            set_nonprofit_total(&transaction, guild_id, nonprofit_total)
        })?;
        transaction.execute(
            "UPDATE loan_state
             SET outstanding_principal = 0,
                 outstanding_fee = 0,
                 total_fees_paid = ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![total_fees_paid, discord_id, guild_id],
        )?;
        transaction.commit()?;

        Ok(LoanRepayment {
            principal: state.outstanding_principal,
            fee: state.outstanding_fee,
            total_owed,
            balance_before,
            new_balance,
            nonprofit_total,
        })
    }
}

fn unique_in_input_order(discord_ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(discord_ids.len());
    discord_ids
        .iter()
        .copied()
        .filter(|discord_id| seen.insert(*discord_id))
        .collect()
}

fn loan_state_in_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<LoanStateRecord>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT discord_id, guild_id, last_loan_at,
                    COALESCE(total_loans_taken, 0),
                    COALESCE(total_fees_paid, 0),
                    COALESCE(negative_loans_taken, 0),
                    COALESCE(outstanding_principal, 0),
                    COALESCE(outstanding_fee, 0)
             FROM loan_state WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| {
                Ok(LoanStateRecord {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    last_loan_at: row.get(2)?,
                    total_loans_taken: row.get(3)?,
                    total_fees_paid: row.get(4)?,
                    negative_loans_taken: row.get(5)?,
                    outstanding_principal: row.get(6)?,
                    outstanding_fee: row.get(7)?,
                })
            },
        )
        .optional()
}

fn player_balance(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0)
             FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn nonprofit_total(transaction: &Transaction<'_>, guild_id: i64) -> Result<i64, rusqlite::Error> {
    Ok(transaction
        .query_row(
            "SELECT COALESCE(total_collected, 0)
             FROM nonprofit_fund WHERE guild_id = ?1",
            params![guild_id],
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
        "INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
         VALUES (?1, ?2, CURRENT_TIMESTAMP)
         ON CONFLICT(guild_id) DO UPDATE SET
             total_collected = excluded.total_collected,
             updated_at = CURRENT_TIMESTAMP",
        params![guild_id, total],
    )?;
    Ok(())
}

fn eligible_bankrupt_ids(
    transaction: &Transaction<'_>,
    discord_ids: &[i64],
    guild_id: i64,
) -> Result<HashSet<i64>, rusqlite::Error> {
    let mut eligible = HashSet::new();
    for chunk in discord_ids.chunks(SQLITE_ID_CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT discord_id FROM players
             WHERE guild_id = ?
               AND discord_id IN ({placeholders})
               AND COALESCE(jopacoin_balance, 0) <= 0"
        );
        let bindings = std::iter::once(guild_id).chain(chunk.iter().copied());
        let mut statement = transaction.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings), |row| row.get(0))?;
        eligible.extend(rows.collect::<Result<Vec<i64>, _>>()?);
    }
    Ok(eligible)
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    context: &LedgerContext,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            context.source,
            context.actor_id,
            context.related_type,
            context.related_id,
            context.reason,
            context.metadata,
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn with_ledger_context<T>(
    transaction: &Transaction<'_>,
    context: &LedgerContext,
    operation: impl FnOnce() -> Result<T, rusqlite::Error>,
) -> Result<T, rusqlite::Error> {
    set_ledger_context(transaction, context)?;
    let outcome = operation();
    let cleared = clear_ledger_context(transaction);
    match outcome {
        Ok(value) => {
            cleared?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

fn with_optional_ledger_context<T>(
    transaction: &Transaction<'_>,
    context: Option<&LedgerContext>,
    operation: impl FnOnce() -> Result<T, rusqlite::Error>,
) -> Result<T, rusqlite::Error> {
    match context.filter(|context| !context.is_empty()) {
        Some(context) => with_ledger_context(transaction, context, operation),
        None => operation(),
    }
}

fn unix_timestamp() -> Result<i64, LoanRepositoryError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LoanRepositoryError::ClockBeforeUnixEpoch)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| LoanRepositoryError::AmountOverflow)
}

#[cfg(test)]
#[path = "loan/tests.rs"]
mod tests;
