//! Discord-independent bankruptcy policy and match-award orchestration.
//!
//! Python remains the live command and migration authority. This module keeps
//! eligibility, cooldown derivation, penalty math, and award ordering behind a
//! typed persistence port. The existing-schema SQLite adapter implements that
//! port without creating or migrating production tables.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::bankruptcy_repository::{
    BankruptcyRepository, BankruptcyRepositoryError,
    DeclarationOutcome as DatabaseDeclarationOutcome,
    DeclarationRequest as DatabaseDeclarationRequest, RawBankruptcyState,
    WinnerAwardRequest as DatabaseWinnerAwardRequest,
};
use thiserror::Error;

pub const NOT_IN_DEBT: &str = "not_in_debt";
pub const BANKRUPTCY_COOLDOWN: &str = "bankruptcy_cooldown";
pub const PLAYER_NOT_FOUND: &str = "player_not_found";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BankruptcyPolicy {
    pub fresh_start_balance: i64,
    pub cooldown_seconds: i64,
    pub penalty_games: i64,
    /// Fraction of a penalized award the player keeps, not the withheld rate.
    pub penalty_kept_rate: f64,
}

impl Default for BankruptcyPolicy {
    fn default() -> Self {
        Self {
            fresh_start_balance: 3,
            cooldown_seconds: 604_800,
            penalty_games: 3,
            penalty_kept_rate: 0.75,
        }
    }
}

impl BankruptcyPolicy {
    pub fn validate(self) -> Result<Self, BankruptcyPolicyError> {
        if self.fresh_start_balance < 0 {
            return Err(BankruptcyPolicyError::InvalidFreshStart(
                self.fresh_start_balance,
            ));
        }
        if self.cooldown_seconds < 0 {
            return Err(BankruptcyPolicyError::InvalidCooldown(
                self.cooldown_seconds,
            ));
        }
        if self.penalty_games < 0 {
            return Err(BankruptcyPolicyError::InvalidPenaltyGames(
                self.penalty_games,
            ));
        }
        if !self.penalty_kept_rate.is_finite() || !(0.0..=1.0).contains(&self.penalty_kept_rate) {
            return Err(BankruptcyPolicyError::InvalidPenaltyRate(
                self.penalty_kept_rate,
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum BankruptcyPolicyError {
    #[error("fresh-start balance cannot be negative: {0}")]
    InvalidFreshStart(i64),
    #[error("cooldown seconds cannot be negative: {0}")]
    InvalidCooldown(i64),
    #[error("penalty games cannot be negative: {0}")]
    InvalidPenaltyGames(i64),
    #[error("bankruptcy kept rate must be finite and within 0..=1: {0}")]
    InvalidPenaltyRate(f64),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoredBankruptcyState {
    pub discord_id: i64,
    pub guild_id: i64,
    pub last_bankruptcy_at: Option<i64>,
    pub penalty_games_remaining: i64,
    pub bankruptcy_count: i64,
}

impl From<RawBankruptcyState> for StoredBankruptcyState {
    fn from(value: RawBankruptcyState) -> Self {
        Self {
            discord_id: value.discord_id,
            guild_id: value.guild_id,
            last_bankruptcy_at: value.last_bankruptcy_at,
            penalty_games_remaining: value.penalty_games_remaining,
            bankruptcy_count: value.bankruptcy_count,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BankruptcyState {
    pub discord_id: i64,
    pub last_bankruptcy_at: Option<i64>,
    pub penalty_games_remaining: i64,
    pub is_on_cooldown: bool,
    /// Present only while the cooldown remains active, matching Python.
    pub cooldown_ends_at: Option<i64>,
    pub bankruptcy_count: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BankruptcyDeclaration {
    pub debt_cleared: i64,
    pub penalty_games: i64,
    pub new_balance: i64,
    pub bankruptcy_count: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankruptcyRejection {
    NotInDebt { balance: i64 },
    Cooldown { remaining_seconds: i64 },
    PlayerNotFound,
}

impl BankruptcyRejection {
    #[must_use]
    pub const fn error_code(self) -> &'static str {
        match self {
            Self::NotInDebt { .. } => NOT_IN_DEBT,
            Self::Cooldown { .. } => BANKRUPTCY_COOLDOWN,
            Self::PlayerNotFound => PLAYER_NOT_FOUND,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankruptcyValidation {
    Eligible { debt: i64 },
    Rejected(BankruptcyRejection),
}

impl BankruptcyValidation {
    #[must_use]
    pub const fn is_eligible(self) -> bool {
        matches!(self, Self::Eligible { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankruptcyExecution {
    Applied(BankruptcyDeclaration),
    Rejected(BankruptcyRejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PenaltyResult {
    pub original: i64,
    pub penalized: i64,
    pub penalty_applied: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AwardReceipt {
    pub discord_id: i64,
    pub gross: i64,
    pub net: i64,
    pub bankruptcy_penalty: i64,
    pub balance_after: i64,
    pub penalty_games_remaining: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtomicAwardRequest<'a> {
    pub discord_ids: &'a [i64],
    pub guild_id: Option<i64>,
    pub gross_reward: i64,
    pub penalty_kept_rate: f64,
    pub decrement_penalty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtomicDeclarationRequest {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub fresh_start_balance: i64,
    pub cooldown_seconds: i64,
    pub penalty_games: i64,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicDeclarationOutcome {
    Applied {
        debt_cleared: i64,
        new_balance: i64,
        penalty_games: i64,
        bankruptcy_count: i64,
    },
    PlayerNotFound,
    NotInDebt {
        balance: i64,
    },
    Cooldown {
        remaining_seconds: i64,
    },
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BankruptcyPortError {
    #[error("bankruptcy storage failed: {0}")]
    Backend(String),
}

impl From<BankruptcyRepositoryError> for BankruptcyPortError {
    fn from(value: BankruptcyRepositoryError) -> Self {
        Self::Backend(value.to_string())
    }
}

/// Persistence and transaction boundary for command and match orchestration.
pub trait BankruptcyPort {
    fn get_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, BankruptcyPortError>;

    fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<StoredBankruptcyState>, BankruptcyPortError>;

    fn get_bulk_states(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, StoredBankruptcyState>, BankruptcyPortError>;

    fn get_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyPortError>;

    fn declare_atomic(
        &self,
        request: AtomicDeclarationRequest,
    ) -> Result<AtomicDeclarationOutcome, BankruptcyPortError>;

    fn decrement_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyPortError>;

    fn decrement_penalty_games_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, BankruptcyPortError>;

    fn award_winners_atomic(
        &self,
        request: AtomicAwardRequest<'_>,
    ) -> Result<Vec<AwardReceipt>, BankruptcyPortError>;
}

impl BankruptcyPort for BankruptcyRepository {
    fn get_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, BankruptcyPortError> {
        BankruptcyRepository::balance(self, discord_id, guild_id).map_err(Into::into)
    }

    fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<StoredBankruptcyState>, BankruptcyPortError> {
        BankruptcyRepository::get_state(self, discord_id, guild_id)
            .map(|state| state.map(Into::into))
            .map_err(Into::into)
    }

    fn get_bulk_states(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, StoredBankruptcyState>, BankruptcyPortError> {
        BankruptcyRepository::get_bulk_states(self, discord_ids, guild_id)
            .map(|states| {
                states
                    .into_iter()
                    .map(|(discord_id, state)| (discord_id, state.into()))
                    .collect()
            })
            .map_err(Into::into)
    }

    fn get_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyPortError> {
        BankruptcyRepository::get_penalty_games(self, discord_id, guild_id).map_err(Into::into)
    }

    fn declare_atomic(
        &self,
        request: AtomicDeclarationRequest,
    ) -> Result<AtomicDeclarationOutcome, BankruptcyPortError> {
        BankruptcyRepository::declare_atomic(
            self,
            DatabaseDeclarationRequest {
                discord_id: request.discord_id,
                guild_id: request.guild_id,
                fresh_start_balance: request.fresh_start_balance,
                cooldown_seconds: request.cooldown_seconds,
                penalty_games: request.penalty_games,
                now: request.now,
            },
        )
        .map(|outcome| match outcome {
            DatabaseDeclarationOutcome::Applied {
                debt_cleared,
                new_balance,
                penalty_games,
                bankruptcy_count,
            } => AtomicDeclarationOutcome::Applied {
                debt_cleared,
                new_balance,
                penalty_games,
                bankruptcy_count,
            },
            DatabaseDeclarationOutcome::PlayerNotFound => AtomicDeclarationOutcome::PlayerNotFound,
            DatabaseDeclarationOutcome::NotInDebt { balance } => {
                AtomicDeclarationOutcome::NotInDebt { balance }
            }
            DatabaseDeclarationOutcome::Cooldown { remaining_seconds } => {
                AtomicDeclarationOutcome::Cooldown { remaining_seconds }
            }
        })
        .map_err(Into::into)
    }

    fn decrement_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyPortError> {
        BankruptcyRepository::decrement_penalty_games(self, discord_id, guild_id)
            .map_err(Into::into)
    }

    fn decrement_penalty_games_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, BankruptcyPortError> {
        BankruptcyRepository::decrement_penalty_games_bulk(self, discord_ids, guild_id)
            .map_err(Into::into)
    }

    fn award_winners_atomic(
        &self,
        request: AtomicAwardRequest<'_>,
    ) -> Result<Vec<AwardReceipt>, BankruptcyPortError> {
        BankruptcyRepository::award_winners_atomic(
            self,
            DatabaseWinnerAwardRequest {
                discord_ids: request.discord_ids,
                guild_id: request.guild_id,
                gross_reward: request.gross_reward,
                penalty_kept_rate: request.penalty_kept_rate,
                decrement_penalty: request.decrement_penalty,
            },
        )
        .map(|receipts| {
            receipts
                .into_iter()
                .map(|receipt| AwardReceipt {
                    discord_id: receipt.discord_id,
                    gross: receipt.gross,
                    net: receipt.net,
                    bankruptcy_penalty: receipt.bankruptcy_penalty,
                    balance_after: receipt.balance_after,
                    penalty_games_remaining: receipt.penalty_games_remaining,
                })
                .collect()
        })
        .map_err(Into::into)
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
pub enum BankruptcyServiceError {
    #[error(transparent)]
    Port(#[from] BankruptcyPortError),
    #[error("bankruptcy clock failed: {0}")]
    Clock(String),
}

#[derive(Clone, Debug)]
pub struct BankruptcyService<P, C = SystemClock> {
    port: P,
    clock: C,
    policy: BankruptcyPolicy,
}

impl<P> BankruptcyService<P, SystemClock> {
    pub fn new(port: P, policy: BankruptcyPolicy) -> Result<Self, BankruptcyPolicyError> {
        Ok(Self {
            port,
            clock: SystemClock,
            policy: policy.validate()?,
        })
    }
}

impl<P, C> BankruptcyService<P, C>
where
    P: BankruptcyPort,
    C: Clock,
{
    pub fn with_clock(
        port: P,
        clock: C,
        policy: BankruptcyPolicy,
    ) -> Result<Self, BankruptcyPolicyError> {
        Ok(Self {
            port,
            clock,
            policy: policy.validate()?,
        })
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    #[must_use]
    pub const fn policy(&self) -> BankruptcyPolicy {
        self.policy
    }

    pub fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BankruptcyState, BankruptcyServiceError> {
        let stored = self.port.get_state(discord_id, guild_id)?;
        let now = self.now()?;
        Ok(derive_state(
            discord_id,
            stored,
            now,
            self.policy.cooldown_seconds,
        ))
    }

    pub fn get_bulk_states(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, BankruptcyState>, BankruptcyServiceError> {
        if discord_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let unique_ids = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        let raw_states = self
            .port
            .get_bulk_states(&unique_ids.iter().copied().collect::<Vec<_>>(), guild_id)?;
        let now = self.now()?;
        Ok(unique_ids
            .into_iter()
            .map(|discord_id| {
                let state = derive_state(
                    discord_id,
                    raw_states.get(&discord_id).copied(),
                    now,
                    self.policy.cooldown_seconds,
                );
                (discord_id, state)
            })
            .collect())
    }

    pub fn validate_bankruptcy(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BankruptcyValidation, BankruptcyServiceError> {
        self.validate_at(discord_id, guild_id, self.now()?)
    }

    pub fn execute_bankruptcy(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BankruptcyExecution, BankruptcyServiceError> {
        let now = self.now()?;
        if let BankruptcyValidation::Rejected(rejection) =
            self.validate_at(discord_id, guild_id, now)?
        {
            return Ok(BankruptcyExecution::Rejected(rejection));
        }
        let outcome = self.port.declare_atomic(AtomicDeclarationRequest {
            discord_id,
            guild_id,
            fresh_start_balance: self.policy.fresh_start_balance,
            cooldown_seconds: self.policy.cooldown_seconds,
            penalty_games: self.policy.penalty_games,
            now,
        })?;
        Ok(match outcome {
            AtomicDeclarationOutcome::Applied {
                debt_cleared,
                new_balance,
                penalty_games,
                bankruptcy_count,
            } => BankruptcyExecution::Applied(BankruptcyDeclaration {
                debt_cleared,
                penalty_games,
                new_balance,
                bankruptcy_count,
            }),
            AtomicDeclarationOutcome::PlayerNotFound => {
                BankruptcyExecution::Rejected(BankruptcyRejection::PlayerNotFound)
            }
            AtomicDeclarationOutcome::NotInDebt { balance } => {
                BankruptcyExecution::Rejected(BankruptcyRejection::NotInDebt { balance })
            }
            AtomicDeclarationOutcome::Cooldown { remaining_seconds } => {
                BankruptcyExecution::Rejected(BankruptcyRejection::Cooldown { remaining_seconds })
            }
        })
    }

    pub fn apply_penalty_to_winnings(
        &self,
        discord_id: i64,
        amount: i64,
        guild_id: Option<i64>,
    ) -> Result<PenaltyResult, BankruptcyServiceError> {
        let penalty_games = self.port.get_penalty_games(discord_id, guild_id)?;
        Ok(calculate_penalty(
            amount,
            penalty_games,
            self.policy.penalty_kept_rate,
        ))
    }

    pub fn on_game_won(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyServiceError> {
        self.port
            .decrement_penalty_games(discord_id, guild_id)
            .map_err(Into::into)
    }

    pub fn on_games_won(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, BankruptcyServiceError> {
        self.port
            .decrement_penalty_games_bulk(discord_ids, guild_id)
            .map_err(Into::into)
    }

    /// Credit winners and consume penalty wins after calculating the award so
    /// the final clearing win is still penalized.
    pub fn award_win_bonus(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        gross_reward: i64,
    ) -> Result<BTreeMap<i64, AwardReceipt>, BankruptcyServiceError> {
        self.award(discord_ids, guild_id, gross_reward, true)
    }

    /// Participation uses the same penalty coefficient but never consumes a
    /// required win.
    pub fn award_participation(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        gross_reward: i64,
    ) -> Result<BTreeMap<i64, AwardReceipt>, BankruptcyServiceError> {
        self.award(discord_ids, guild_id, gross_reward, false)
    }

    fn validate_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<BankruptcyValidation, BankruptcyServiceError> {
        let Some(balance) = self.port.get_balance(discord_id, guild_id)? else {
            return Ok(BankruptcyValidation::Rejected(
                BankruptcyRejection::PlayerNotFound,
            ));
        };
        if balance >= 0 {
            return Ok(BankruptcyValidation::Rejected(
                BankruptcyRejection::NotInDebt { balance },
            ));
        }
        let state = derive_state(
            discord_id,
            self.port.get_state(discord_id, guild_id)?,
            now,
            self.policy.cooldown_seconds,
        );
        if state.is_on_cooldown {
            return Ok(BankruptcyValidation::Rejected(
                BankruptcyRejection::Cooldown {
                    remaining_seconds: state.cooldown_ends_at.unwrap_or(now).saturating_sub(now),
                },
            ));
        }
        Ok(BankruptcyValidation::Eligible {
            debt: balance.saturating_abs(),
        })
    }

    fn award(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        gross_reward: i64,
        decrement_penalty: bool,
    ) -> Result<BTreeMap<i64, AwardReceipt>, BankruptcyServiceError> {
        let receipts = self.port.award_winners_atomic(AtomicAwardRequest {
            discord_ids,
            guild_id,
            gross_reward,
            penalty_kept_rate: self.policy.penalty_kept_rate,
            decrement_penalty,
        })?;
        Ok(receipts
            .into_iter()
            .map(|receipt| (receipt.discord_id, receipt))
            .collect())
    }

    fn now(&self) -> Result<i64, BankruptcyServiceError> {
        self.clock
            .unix_timestamp()
            .map_err(BankruptcyServiceError::Clock)
    }
}

#[must_use]
pub fn derive_state(
    discord_id: i64,
    stored: Option<StoredBankruptcyState>,
    now: i64,
    cooldown_seconds: i64,
) -> BankruptcyState {
    let Some(stored) = stored else {
        return BankruptcyState {
            discord_id,
            last_bankruptcy_at: None,
            penalty_games_remaining: 0,
            is_on_cooldown: false,
            cooldown_ends_at: None,
            bankruptcy_count: 0,
        };
    };
    let cooldown_end = stored
        .last_bankruptcy_at
        .filter(|timestamp| *timestamp != 0)
        .map(|timestamp| timestamp.saturating_add(cooldown_seconds));
    let is_on_cooldown = cooldown_end.is_some_and(|end| now < end);
    BankruptcyState {
        discord_id,
        last_bankruptcy_at: stored.last_bankruptcy_at,
        penalty_games_remaining: stored.penalty_games_remaining,
        is_on_cooldown,
        cooldown_ends_at: cooldown_end.filter(|_| is_on_cooldown),
        bankruptcy_count: stored.bankruptcy_count,
    }
}

/// Match Python's positive-award behavior: floor the amount withheld, not the
/// amount kept, so a one-coin award cannot round all the way down to zero.
#[must_use]
pub fn calculate_penalty(amount: i64, penalty_games: i64, kept_rate: f64) -> PenaltyResult {
    if penalty_games <= 0 {
        return PenaltyResult {
            original: amount,
            penalized: amount,
            penalty_applied: 0,
        };
    }
    let penalty_applied = (amount as f64 * (1.0 - kept_rate)) as i64;
    PenaltyResult {
        original: amount,
        penalized: amount.saturating_sub(penalty_applied),
        penalty_applied,
    }
}

#[cfg(test)]
mod tests;
