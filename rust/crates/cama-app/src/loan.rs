//! Discord-independent loan orchestration and match-repayment hook.
//!
//! Python remains the production runtime. This module keeps policy, result
//! mapping, and match-finalization ordering behind typed ports; the existing-
//! schema [`cama_db::loan_repository::LoanRepository`] implements the port for
//! side-by-side verification without claiming a runtime cutover.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::loan_repository::{
    LedgerContext, LoanExecution as DatabaseLoanExecution, LoanRepayment as DatabaseLoanRepayment,
    LoanRepository, LoanRepositoryError, LoanStateRecord, StipendPayment as DatabaseStipendPayment,
};
use thiserror::Error;

pub const LOAN_ALREADY_EXISTS: &str = "loan_already_exists";
pub const COOLDOWN_ACTIVE: &str = "cooldown_active";
pub const VALIDATION_ERROR: &str = "validation_error";
pub const LOAN_AMOUNT_EXCEEDED: &str = "loan_amount_exceeded";
pub const PLAYER_NOT_FOUND: &str = "player_not_found";
pub const NO_OUTSTANDING_LOAN: &str = "no_outstanding_loan";
pub const STORAGE_ERROR: &str = "storage_error";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoanPolicy {
    pub cooldown_seconds: i64,
    pub max_amount: i64,
    pub fee_rate: f64,
    /// Retained for Python configuration compatibility. Current loan policy,
    /// like Python, does not enforce this value during issuance or repayment.
    pub max_debt: i64,
}

impl Default for LoanPolicy {
    fn default() -> Self {
        Self {
            cooldown_seconds: 259_200,
            max_amount: 100,
            fee_rate: 0.20,
            max_debt: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoredLoanState {
    pub discord_id: i64,
    pub last_loan_at: Option<i64>,
    pub total_loans_taken: i64,
    pub total_fees_paid: i64,
    pub negative_loans_taken: i64,
    pub outstanding_principal: i64,
    pub outstanding_fee: i64,
}

impl From<LoanStateRecord> for StoredLoanState {
    fn from(value: LoanStateRecord) -> Self {
        Self {
            discord_id: value.discord_id,
            last_loan_at: value.last_loan_at,
            total_loans_taken: value.total_loans_taken,
            total_fees_paid: value.total_fees_paid,
            negative_loans_taken: value.negative_loans_taken,
            outstanding_principal: value.outstanding_principal,
            outstanding_fee: value.outstanding_fee,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoanState {
    pub discord_id: i64,
    pub last_loan_at: Option<i64>,
    pub total_loans_taken: i64,
    pub total_fees_paid: i64,
    pub negative_loans_taken: i64,
    pub is_on_cooldown: bool,
    pub cooldown_ends_at: Option<i64>,
    pub outstanding_principal: i64,
    pub outstanding_fee: i64,
}

impl LoanState {
    #[must_use]
    pub fn has_outstanding_loan(self) -> bool {
        self.outstanding_principal > 0
    }

    #[must_use]
    pub fn outstanding_total(self) -> Option<i64> {
        self.outstanding_principal.checked_add(self.outstanding_fee)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoanApproval {
    pub amount: i64,
    pub fee: i64,
    pub total_owed: i64,
    pub new_balance: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoanResult {
    pub amount: i64,
    pub fee: i64,
    pub total_owed: i64,
    pub new_balance: i64,
    pub total_loans_taken: i64,
    pub was_negative_loan: bool,
}

impl From<DatabaseLoanExecution> for LoanResult {
    fn from(value: DatabaseLoanExecution) -> Self {
        Self {
            amount: value.amount,
            fee: value.fee,
            total_owed: value.total_owed,
            new_balance: value.new_balance,
            total_loans_taken: value.total_loans_taken,
            was_negative_loan: value.was_negative_loan,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepaymentResult {
    pub principal: i64,
    pub fee: i64,
    pub total_repaid: i64,
    pub balance_before: i64,
    pub new_balance: i64,
    pub nonprofit_total: i64,
}

impl From<DatabaseLoanRepayment> for RepaymentResult {
    fn from(value: DatabaseLoanRepayment) -> Self {
        Self {
            principal: value.principal,
            fee: value.fee,
            total_repaid: value.total_owed,
            balance_before: value.balance_before,
            new_balance: value.new_balance,
            nonprofit_total: value.nonprofit_total,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StipendPayment {
    pub discord_id: i64,
    pub amount: i64,
}

impl From<DatabaseStipendPayment> for StipendPayment {
    fn from(value: DatabaseStipendPayment) -> Self {
        Self {
            discord_id: value.discord_id,
            amount: value.amount,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtomicLoanRequest {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub amount: i64,
    pub fee: i64,
    pub cooldown_seconds: i64,
    pub max_amount: i64,
    pub now: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferContext {
    pub source: Option<String>,
    pub actor_id: Option<i64>,
    pub related_type: Option<String>,
    pub related_id: Option<String>,
    pub reason: Option<String>,
    /// Already-serialized JSON, matching Python's string-or-dict repository
    /// boundary while keeping this port independent of a JSON dependency.
    pub metadata: Option<String>,
}

impl From<&TransferContext> for LedgerContext {
    fn from(value: &TransferContext) -> Self {
        Self {
            source: value.source.clone(),
            actor_id: value.actor_id,
            related_type: value.related_type.clone(),
            related_id: value.related_id.clone(),
            reason: value.reason.clone(),
            metadata: value.metadata.clone(),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LoanPortError {
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
    #[error("{0}")]
    Backend(String),
}

impl From<LoanRepositoryError> for LoanPortError {
    fn from(value: LoanRepositoryError) -> Self {
        match value {
            LoanRepositoryError::OutstandingLoan {
                principal,
                fee,
                total_owed,
            } => Self::OutstandingLoan {
                principal,
                fee,
                total_owed,
            },
            LoanRepositoryError::CooldownActive { hours, minutes } => {
                Self::CooldownActive { hours, minutes }
            }
            LoanRepositoryError::InvalidLoanAmount => Self::InvalidLoanAmount,
            LoanRepositoryError::LoanAmountExceeded { max_amount } => {
                Self::LoanAmountExceeded { max_amount }
            }
            LoanRepositoryError::PlayerNotFound => Self::PlayerNotFound,
            LoanRepositoryError::NoOutstandingLoan => Self::NoOutstandingLoan,
            other => Self::Backend(other.to_string()),
        }
    }
}

/// Persistence boundary used by both command orchestration and match cleanup.
pub trait LoanPort {
    fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<StoredLoanState>, LoanPortError>;

    /// Python's player repository returns zero when the row is absent.
    fn get_balance(&self, discord_id: i64, guild_id: Option<i64>) -> Result<i64, LoanPortError>;

    fn execute_loan_atomic(&self, request: AtomicLoanRequest) -> Result<LoanResult, LoanPortError>;

    fn execute_repayment_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<RepaymentResult, LoanPortError>;

    fn get_outstanding_borrower_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashSet<i64>, LoanPortError>;

    fn get_nonprofit_fund(&self, guild_id: Option<i64>) -> Result<i64, LoanPortError>;

    fn distribute_nonprofit_stipends_atomic(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        max_stipend: i64,
    ) -> Result<Vec<StipendPayment>, LoanPortError>;

    fn transfer_balance_to_nonprofit_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        context: &TransferContext,
    ) -> Result<i64, LoanPortError>;

    fn reset_cooldown(&self, discord_id: i64, guild_id: Option<i64>) -> Result<(), LoanPortError>;
}

impl LoanPort for LoanRepository {
    fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<StoredLoanState>, LoanPortError> {
        LoanRepository::get_state(self, discord_id, guild_id)
            .map(|state| state.map(Into::into))
            .map_err(Into::into)
    }

    fn get_balance(&self, discord_id: i64, guild_id: Option<i64>) -> Result<i64, LoanPortError> {
        LoanRepository::get_player_balance(self, discord_id, guild_id)
            .map(|balance| balance.unwrap_or(0))
            .map_err(Into::into)
    }

    fn execute_loan_atomic(&self, request: AtomicLoanRequest) -> Result<LoanResult, LoanPortError> {
        LoanRepository::execute_loan_atomic_at(
            self,
            request.discord_id,
            request.guild_id,
            request.amount,
            request.fee,
            request.cooldown_seconds,
            request.max_amount,
            request.now,
        )
        .map(Into::into)
        .map_err(Into::into)
    }

    fn execute_repayment_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<RepaymentResult, LoanPortError> {
        LoanRepository::execute_repayment_atomic(self, discord_id, guild_id)
            .map(Into::into)
            .map_err(Into::into)
    }

    fn get_outstanding_borrower_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashSet<i64>, LoanPortError> {
        LoanRepository::get_outstanding_borrower_ids(self, discord_ids, guild_id)
            .map_err(Into::into)
    }

    fn get_nonprofit_fund(&self, guild_id: Option<i64>) -> Result<i64, LoanPortError> {
        LoanRepository::get_nonprofit_fund(self, guild_id).map_err(Into::into)
    }

    fn distribute_nonprofit_stipends_atomic(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        max_stipend: i64,
    ) -> Result<Vec<StipendPayment>, LoanPortError> {
        LoanRepository::distribute_nonprofit_stipends_atomic(
            self,
            discord_ids,
            guild_id,
            max_stipend,
        )
        .map(|payments| payments.into_iter().map(Into::into).collect())
        .map_err(Into::into)
    }

    fn transfer_balance_to_nonprofit_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        context: &TransferContext,
    ) -> Result<i64, LoanPortError> {
        LoanRepository::transfer_balance_to_nonprofit_atomic(
            self,
            discord_id,
            guild_id,
            amount,
            &LedgerContext::from(context),
        )
        .map_err(Into::into)
    }

    fn reset_cooldown(&self, discord_id: i64, guild_id: Option<i64>) -> Result<(), LoanPortError> {
        LoanRepository::reset_cooldown(self, discord_id, guild_id).map_err(Into::into)
    }
}

pub trait Clock {
    fn unix_timestamp(&self) -> Result<i64, String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_timestamp(&self) -> Result<i64, String> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is before the Unix epoch".to_owned())?
            .as_secs();
        i64::try_from(seconds).map_err(|_| "system clock exceeds signed 64-bit range".to_owned())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct LoanServiceError {
    pub message: String,
    pub code: &'static str,
}

impl LoanServiceError {
    fn new(message: impl Into<String>, code: &'static str) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }

    fn storage(message: impl Into<String>) -> Self {
        Self::new(message, STORAGE_ERROR)
    }
}

fn map_port_error(error: LoanPortError) -> LoanServiceError {
    let code = match error {
        LoanPortError::OutstandingLoan { .. } => LOAN_ALREADY_EXISTS,
        LoanPortError::CooldownActive { .. } => COOLDOWN_ACTIVE,
        LoanPortError::InvalidLoanAmount => VALIDATION_ERROR,
        LoanPortError::LoanAmountExceeded { .. } => LOAN_AMOUNT_EXCEEDED,
        LoanPortError::PlayerNotFound => PLAYER_NOT_FOUND,
        LoanPortError::NoOutstandingLoan => NO_OUTSTANDING_LOAN,
        LoanPortError::Backend(_) => STORAGE_ERROR,
    };
    LoanServiceError::new(error.to_string(), code)
}

#[derive(Clone, Debug)]
pub struct LoanService<P, C = SystemClock> {
    port: P,
    clock: C,
    policy: LoanPolicy,
}

impl<P> LoanService<P, SystemClock> {
    #[must_use]
    pub fn new(port: P, policy: LoanPolicy) -> Self {
        Self {
            port,
            clock: SystemClock,
            policy,
        }
    }
}

impl<P, C> LoanService<P, C>
where
    P: LoanPort,
    C: Clock,
{
    #[must_use]
    pub const fn with_clock(port: P, clock: C, policy: LoanPolicy) -> Self {
        Self {
            port,
            clock,
            policy,
        }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<LoanState, LoanServiceError> {
        let stored = self
            .port
            .get_state(discord_id, guild_id)
            .map_err(map_port_error)?;
        let now = self
            .clock
            .unix_timestamp()
            .map_err(LoanServiceError::storage)?;
        Ok(derive_state(
            discord_id,
            stored,
            now,
            self.policy.cooldown_seconds,
        ))
    }

    pub fn get_outstanding_borrower_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashSet<i64>, LoanServiceError> {
        self.port
            .get_outstanding_borrower_ids(discord_ids, guild_id)
            .map_err(map_port_error)
    }

    pub fn get_nonprofit_fund(&self, guild_id: Option<i64>) -> Result<i64, LoanServiceError> {
        self.port
            .get_nonprofit_fund(guild_id)
            .map_err(map_port_error)
    }

    pub fn distribute_nonprofit_stipends(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        max_stipend: i64,
    ) -> Result<Vec<StipendPayment>, LoanServiceError> {
        self.port
            .distribute_nonprofit_stipends_atomic(discord_ids, guild_id, max_stipend)
            .map_err(map_port_error)
    }

    pub fn transfer_balance_to_nonprofit(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        context: &TransferContext,
    ) -> Result<i64, LoanServiceError> {
        self.port
            .transfer_balance_to_nonprofit_atomic(discord_id, guild_id, amount, context)
            .map_err(map_port_error)
    }

    pub fn reset_loan_cooldown(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), LoanServiceError> {
        self.port
            .reset_cooldown(discord_id, guild_id)
            .map_err(map_port_error)
    }

    pub fn validate_loan(
        &self,
        discord_id: i64,
        amount: i64,
        guild_id: Option<i64>,
        fee_rate_override: Option<f64>,
        max_amount_override: Option<i64>,
    ) -> Result<LoanApproval, LoanServiceError> {
        let state = self.get_state(discord_id, guild_id)?;
        let balance = self
            .port
            .get_balance(discord_id, guild_id)
            .map_err(map_port_error)?;
        if state.has_outstanding_loan() {
            let total = state.outstanding_total().ok_or_else(|| {
                LoanServiceError::new("Loan total exceeds signed 64-bit range.", VALIDATION_ERROR)
            })?;
            return Err(LoanServiceError::new(
                format!(
                    "You have an outstanding loan of {total} (principal: {}, fee: {}). Repay it by playing in a match first!",
                    state.outstanding_principal, state.outstanding_fee
                ),
                LOAN_ALREADY_EXISTS,
            ));
        }
        if state.is_on_cooldown {
            let remaining = state.cooldown_ends_at.unwrap_or(0).saturating_sub(
                self.clock
                    .unix_timestamp()
                    .map_err(LoanServiceError::storage)?,
            );
            return Err(LoanServiceError::new(
                format!(
                    "Loan cooldown active. Try again in {}h {}m.",
                    remaining / 3_600,
                    (remaining % 3_600) / 60
                ),
                COOLDOWN_ACTIVE,
            ));
        }
        if amount <= 0 {
            return Err(LoanServiceError::new(
                "Loan amount must be positive.",
                VALIDATION_ERROR,
            ));
        }
        let max_amount = max_amount_override.unwrap_or(self.policy.max_amount);
        if amount > max_amount {
            return Err(LoanServiceError::new(
                format!("Maximum loan amount is {max_amount}."),
                LOAN_AMOUNT_EXCEEDED,
            ));
        }
        let fee_rate = fee_rate_override.unwrap_or(self.policy.fee_rate);
        let fee = calculate_fee(amount, fee_rate)?;
        let total_owed = amount.checked_add(fee).ok_or_else(|| {
            LoanServiceError::new("Loan total exceeds signed 64-bit range.", VALIDATION_ERROR)
        })?;
        let new_balance = balance.checked_add(amount).ok_or_else(|| {
            LoanServiceError::new(
                "Loan balance exceeds signed 64-bit range.",
                VALIDATION_ERROR,
            )
        })?;
        Ok(LoanApproval {
            amount,
            fee,
            total_owed,
            new_balance,
        })
    }

    pub fn execute_loan(
        &self,
        discord_id: i64,
        amount: i64,
        guild_id: Option<i64>,
        fee_rate_override: Option<f64>,
        max_amount_override: Option<i64>,
    ) -> Result<LoanResult, LoanServiceError> {
        let fee_rate = fee_rate_override.unwrap_or(self.policy.fee_rate);
        let fee = calculate_fee(amount, fee_rate)?;
        let now = self
            .clock
            .unix_timestamp()
            .map_err(LoanServiceError::storage)?;
        self.port
            .execute_loan_atomic(AtomicLoanRequest {
                discord_id,
                guild_id,
                amount,
                fee,
                cooldown_seconds: self.policy.cooldown_seconds,
                max_amount: max_amount_override.unwrap_or(self.policy.max_amount),
                now,
            })
            .map_err(map_port_error)
    }

    pub fn execute_repayment(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<RepaymentResult, LoanServiceError> {
        self.port
            .execute_repayment_atomic(discord_id, guild_id)
            .map_err(map_port_error)
    }

    /// Match-finalization hook: one bulk discovery, stable participant order,
    /// at most one repayment attempt per borrower, and only successes returned.
    pub fn repay_outstanding_match_loans(
        &self,
        participant_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<MatchLoanRepayment>, LoanServiceError> {
        let borrower_ids = self.get_outstanding_borrower_ids(participant_ids, guild_id)?;
        let mut attempted_ids = HashSet::new();
        let mut repayments = Vec::new();
        for player_id in participant_ids.iter().copied() {
            if !borrower_ids.contains(&player_id) || !attempted_ids.insert(player_id) {
                continue;
            }
            let Ok(repayment) = self.execute_repayment(player_id, guild_id) else {
                continue;
            };
            repayments.push(MatchLoanRepayment {
                player_id,
                principal: repayment.principal,
                fee: repayment.fee,
                total_repaid: repayment.total_repaid,
                balance_before: repayment.balance_before,
                new_balance: repayment.new_balance,
                nonprofit_total: repayment.nonprofit_total,
            });
        }
        Ok(repayments)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchLoanRepayment {
    pub player_id: i64,
    pub principal: i64,
    pub fee: i64,
    pub total_repaid: i64,
    pub balance_before: i64,
    pub new_balance: i64,
    pub nonprofit_total: i64,
}

#[must_use]
pub fn derive_state(
    discord_id: i64,
    stored: Option<StoredLoanState>,
    now: i64,
    cooldown_seconds: i64,
) -> LoanState {
    let stored = stored.unwrap_or(StoredLoanState {
        discord_id,
        ..StoredLoanState::default()
    });
    let cooldown_ends = stored
        .last_loan_at
        .filter(|last_loan_at| *last_loan_at != 0)
        .map(|last_loan_at| last_loan_at.saturating_add(cooldown_seconds));
    let is_on_cooldown = cooldown_ends.is_some_and(|ends_at| now < ends_at);
    LoanState {
        discord_id,
        last_loan_at: stored.last_loan_at,
        total_loans_taken: stored.total_loans_taken,
        total_fees_paid: stored.total_fees_paid,
        negative_loans_taken: stored.negative_loans_taken,
        is_on_cooldown,
        cooldown_ends_at: cooldown_ends.filter(|_| is_on_cooldown),
        outstanding_principal: stored.outstanding_principal,
        outstanding_fee: stored.outstanding_fee,
    }
}

fn calculate_fee(amount: i64, rate: f64) -> Result<i64, LoanServiceError> {
    let fee = (amount as f64) * rate;
    if !fee.is_finite() || fee < i64::MIN as f64 || fee > i64::MAX as f64 {
        return Err(LoanServiceError::new(
            "Loan fee is outside the signed 64-bit range.",
            VALIDATION_ERROR,
        ));
    }
    Ok(fee.trunc() as i64)
}

#[cfg(test)]
#[path = "loan/tests.rs"]
mod tests;
