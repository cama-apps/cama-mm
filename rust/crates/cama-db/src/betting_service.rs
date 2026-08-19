//! Existing-schema persistence for match betting.
//!
//! Python remains the schema, migration, and live runtime authority. This
//! module opens only an already-migrated database through
//! [`crate::open_runtime_connection`]. Bet placement, automatic batches,
//! refunds, settlement, and one-shot buff credits use `BEGIN IMMEDIATE` so a
//! balance change cannot be separated from the row that explains it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::dota_bet_seed_repository::{
    BettingMode, BettingTeam, DotaBetSeedRepositoryError, SeedSettlement,
    settle_seed_in_transaction,
};
use crate::open_runtime_connection;

#[derive(Debug, Error)]
pub enum BettingServiceRepositoryError {
    #[error("betting SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("bet amount and leverage must be positive")]
    InvalidWager,
    #[error("pending match {pending_match_id} does not exist in guild {guild_id}")]
    MissingPendingMatch {
        guild_id: i64,
        pending_match_id: i64,
    },
    #[error("pending match payload is missing a valid betting lock")]
    InvalidPendingPayload,
    #[error("betting is closed")]
    BettingClosed,
    #[error("participant {discord_id} may only bet on {required_team}")]
    ParticipantTeamOnly {
        discord_id: i64,
        required_team: &'static str,
    },
    #[error("opposite-side bets require a draft spectator")]
    OppositeTeamNotAllowed,
    #[error("player {discord_id} is not registered in guild {guild_id}")]
    MissingPlayer { discord_id: i64, guild_id: i64 },
    #[error("player {0} cannot bet while in debt")]
    PlayerInDebt(i64),
    #[error("player {discord_id} has insufficient balance {balance} for stake {stake}")]
    InsufficientBalance {
        discord_id: i64,
        balance: i64,
        stake: i64,
    },
    #[error("player {discord_id} would exceed the maximum debt {max_debt}")]
    DebtLimit { discord_id: i64, max_debt: i64 },
    #[error("wager arithmetic overflowed")]
    WagerOverflow,
    #[error("automatic wager already exists for player {0}")]
    AutomaticBetAlreadyExists(i64),
    #[error("match {match_id} does not exist in guild {guild_id}")]
    MissingMatch { match_id: i64, guild_id: i64 },
    #[error("match betting snapshot JSON is invalid: {0}")]
    InvalidSnapshot(String),
    #[error("betting seed settlement failed: {0}")]
    Seed(#[from] DotaBetSeedRepositoryError),
}

#[derive(Clone, Copy, Debug)]
pub struct PlaceBetRequest {
    pub guild_id: Option<i64>,
    pub pending_match_id: i64,
    pub discord_id: i64,
    pub team: BettingTeam,
    pub amount: i64,
    pub bet_time: i64,
    pub leverage: i64,
    pub max_debt: i64,
    pub is_blind: bool,
    pub odds_at_placement: Option<f64>,
}

/// Optional attribution carried by an automatic investment wager.
///
/// Keeping this separate from [`PlaceBetRequest`] preserves the small, copyable
/// manual-wager request while still making the nullable Python columns an
/// explicit, validated API for callers that persist configured investments.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BetAttribution {
    pub investment_target_id: Option<i64>,
    pub investment_direction: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct AutomaticBetRequest {
    pub discord_id: i64,
    pub team: BettingTeam,
    /// Integer percentage of the balance snapshot, e.g. `10` means 10%.
    pub percentage: i64,
}

/// Exact spectator wager expressed in basis points of the locked balance
/// snapshot.  `100` is one percent and the amount is rounded with Python's
/// ties-to-even rule inside the write transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomaticBasisPointBetRequest {
    pub discord_id: i64,
    pub team: BettingTeam,
    pub basis_points: i64,
}

/// Exact automatic wager amount.  This is the persistence counterpart for
/// callers that have already applied their own policy rounding.  Each
/// request is idempotent for one pending match and un-attributed automatic
/// wager, so a retry cannot debit or insert a second row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomaticAmountBetRequest {
    pub discord_id: i64,
    pub team: BettingTeam,
    pub amount: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlindBetCandidate {
    pub discord_id: i64,
    pub team: BettingTeam,
    pub ante_override: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlindBetPolicy {
    pub is_bomb_pot: bool,
    pub normal_threshold: i64,
    pub normal_percentage: i64,
    pub bomb_percentage: i64,
    pub bomb_ante: i64,
    pub max_debt: i64,
}

impl Default for BlindBetPolicy {
    fn default() -> Self {
        Self {
            is_bomb_pot: false,
            normal_threshold: 50,
            normal_percentage: 10,
            bomb_percentage: 15,
            bomb_ante: 10,
            max_debt: 500,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlindBetSkip {
    pub discord_id: i64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlindBetOutcome {
    pub created: Vec<PendingBetRecord>,
    pub skipped: Vec<BlindBetSkip>,
    pub balance_snapshot: BTreeMap<i64, i64>,
    pub total_radiant: i64,
    pub total_dire: i64,
    pub is_bomb_pot: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingBetRecord {
    pub bet_id: i64,
    pub guild_id: i64,
    pub discord_id: i64,
    pub team: BettingTeam,
    pub amount: i64,
    pub bet_time: i64,
    pub leverage: i64,
    pub is_blind: bool,
    pub odds_at_placement: Option<f64>,
    pub pending_match_id: Option<i64>,
    pub investment_target_id: Option<i64>,
    pub investment_direction: Option<String>,
}

impl PendingBetRecord {
    #[must_use]
    pub fn effective_amount(&self) -> i64 {
        self.amount.saturating_mul(self.leverage)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutomaticBetOutcome {
    pub created: Vec<PendingBetRecord>,
    pub skipped: Vec<i64>,
    pub balance_snapshot: BTreeMap<i64, i64>,
}

/// The durable side effects that Python records after a successful `/bet`.
///
/// Keeping this receipt in the betting repository gives the runtime Neon
/// adapter one consistent snapshot for its rare triggers without reading a
/// player, incrementing the counter, and deciding a trigger in three
/// unrelated transactions.  `balance_after` is the post-wager wallet value;
/// callers that need Python's pre-wager display value add the base stake (not
/// the leveraged effective stake), matching `bet_actions.py`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BetCounterReceipt {
    pub total_bets_placed: i64,
    pub balance_after: i64,
    pub first_leverage_candidate: bool,
}

/// Owned request object for Match's configured-investment hook.  It is
/// intentionally composed only of existing-schema IDs and values so Match
/// can call it at shuffle creation without borrowing provider-local state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredInvestmentRequest {
    pub guild_id: Option<i64>,
    pub pending_match_id: Option<i64>,
    pub bet_time: i64,
    pub radiant_ids: Vec<i64>,
    pub dire_ids: Vec<i64>,
    pub max_debt: i64,
    /// Wallet values captured before Match creates its automatic blind bets.
    /// An empty map retains the standalone repository method's historical
    /// behavior and uses the locked balance snapshot instead.
    pub starting_balances: BTreeMap<i64, i64>,
}

struct ConfiguredInvestmentBatch<'a> {
    guild_id: Option<i64>,
    pending_match_id: Option<i64>,
    bet_time: i64,
    radiant_ids: &'a [i64],
    dire_ids: &'a [i64],
    max_debt: i64,
    starting_balances: Option<&'a BTreeMap<i64, i64>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PotTotals {
    pub radiant: i64,
    pub dire: i64,
}

impl PotTotals {
    #[must_use]
    pub const fn total(self) -> i64 {
        self.radiant.saturating_add(self.dire)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettledBet {
    pub bet_id: i64,
    pub discord_id: i64,
    pub team: BettingTeam,
    pub effective_amount: i64,
    pub payout: i64,
    pub balance_after: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettlementResult {
    pub winners: Vec<SettledBet>,
    pub losers: Vec<SettledBet>,
    pub bankruptcy_penalties: BTreeMap<i64, i64>,
    pub vanity_taxes: BTreeMap<i64, i64>,
    pub low_priority_taxes: BTreeMap<i64, i64>,
    pub seed: SeedSettlement,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BetSettlementAdjustments {
    /// Fraction of positive profit kept by a persisted penalized player.
    /// `None` skips the bankruptcy-state lookup.
    pub bankruptcy_keep_basis_points: Option<i64>,
    /// Exact floating-point policy used by production configuration. When
    /// present this takes precedence over the compatibility basis-point field.
    pub bankruptcy_kept_rate: Option<f64>,
    /// Profit-only vanity tax applied to `vanity_taxable_ids`.
    pub vanity_tax_basis_points: i64,
    /// Exact production vanity rate; takes precedence when present.
    pub vanity_tax_rate: Option<f64>,
    pub vanity_taxable_ids: BTreeSet<i64>,
    /// Profit-only low-priority tax applied to `low_priority_taxable_ids`.
    /// `None` disables it, mirroring an absent vanity rate.
    pub low_priority_tax_rate: Option<f64>,
    pub low_priority_taxable_ids: BTreeSet<i64>,
    /// Python-compatible house profit multiple. `1.0` returns stake + one
    /// stake of profit; invalid values fall back to `1.0`.
    pub house_payout_multiplier: f64,
    /// Active economy-event multiplier applied to the final gross payout once
    /// per user. Invalid values fall back to `1.0` and valid values clamp to
    /// Python's `0..=10` disclosure range.
    pub payout_multiplier: f64,
    /// Consume the pending match's durable reserve seed in this transaction.
    pub consume_seed: bool,
}

impl Default for BetSettlementAdjustments {
    fn default() -> Self {
        Self {
            bankruptcy_keep_basis_points: None,
            bankruptcy_kept_rate: None,
            vanity_tax_basis_points: 0,
            vanity_tax_rate: None,
            vanity_taxable_ids: BTreeSet::new(),
            low_priority_tax_rate: None,
            low_priority_taxable_ids: BTreeSet::new(),
            house_payout_multiplier: 1.0,
            payout_multiplier: 1.0,
            consume_seed: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetHistoryEntry {
    pub bet_id: i64,
    pub match_id: i64,
    pub team: BettingTeam,
    pub effective_amount: i64,
    pub payout: Option<i64>,
    pub won: bool,
    pub profit: i64,
}

#[derive(Clone, Debug)]
pub struct BettingServiceRepository {
    path: PathBuf,
}

impl BettingServiceRepository {
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

    pub fn place_bet_atomic(
        &self,
        request: PlaceBetRequest,
    ) -> Result<PendingBetRecord, BettingServiceRepositoryError> {
        self.place_bet_atomic_with_attribution(request, None)
    }

    /// Place one wager with optional automatic-investment attribution.
    ///
    /// Python treats the two attribution columns as a pair: a direction is
    /// meaningless without a target, an attributed wager must be blind, and
    /// only `long`/`short` are accepted.  Validate that contract before
    /// opening the SQLite transaction so every rejection leaves the wallet and
    /// bet table untouched.
    pub fn place_bet_atomic_with_attribution(
        &self,
        request: PlaceBetRequest,
        attribution: Option<BetAttribution>,
    ) -> Result<PendingBetRecord, BettingServiceRepositoryError> {
        validate_wager(request.amount, request.leverage)?;
        validate_bet_attribution(request.is_blind, attribution.as_ref())?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload = transaction
            .query_row(
                "SELECT payload FROM pending_matches
                 WHERE pending_match_id = ?1 AND guild_id = ?2",
                params![request.pending_match_id, guild_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(BettingServiceRepositoryError::MissingPendingMatch {
                guild_id,
                pending_match_id: request.pending_match_id,
            })?;
        validate_pending_bet(&transaction, &payload, request)?;
        let record = insert_bet_in_transaction_with_attribution(
            &transaction,
            guild_id,
            request,
            false,
            false,
            attribution.as_ref().map(|value| {
                (
                    value.investment_target_id.expect("validated target"),
                    value
                        .investment_direction
                        .as_deref()
                        .expect("validated direction"),
                )
            }),
        )?;
        transaction.commit()?;
        Ok(record)
    }

    /// Record the post-commit `/bet` counter side effect and return the
    /// durable snapshot consumed by the rare Neon triggers.
    ///
    /// This deliberately remains a separate transaction from
    /// [`Self::place_bet_atomic`]: the Python command increments the counter
    /// only after the wager has committed and after its success response is
    /// being handled.  `BEGIN IMMEDIATE` still makes concurrent successful
    /// bets serialize their counter values, so the 100-bet boundary cannot be
    /// lost to a read/modify/write race.
    pub fn record_successful_bet_counter(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        leverage: i64,
    ) -> Result<BetCounterReceipt, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0),
                        COALESCE(total_bets_placed, 0),
                        COALESCE(first_leverage_used, 0)
                 FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(BettingServiceRepositoryError::MissingPlayer {
                discord_id,
                guild_id,
            })?;
        let changed = transaction.execute(
            "UPDATE players
             SET total_bets_placed = COALESCE(total_bets_placed, 0) + 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
        if changed == 0 {
            return Err(BettingServiceRepositoryError::MissingPlayer {
                discord_id,
                guild_id,
            });
        }
        transaction.commit()?;
        Ok(BetCounterReceipt {
            total_bets_placed: current.1.saturating_add(1),
            balance_after: current.0,
            first_leverage_candidate: leverage > 1 && current.2 == 0,
        })
    }

    /// Atomically claim the durable first-leverage marker after its Neon
    /// event has actually fired.  Returning `false` is intentionally benign
    /// for a missing/already-marked player, matching Python's `rowcount > 0`
    /// contract and allowing the event delivery path to stay best-effort.
    pub fn mark_first_leverage_used(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE players
             SET first_leverage_used = 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2
               AND (first_leverage_used IS NULL OR first_leverage_used = 0)",
            params![discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    /// Place one configured long/short investment and retain its attribution
    /// on the existing `bets` row.  Match settlement and gambling statistics
    /// already consume these nullable columns; keeping the write here avoids
    /// a second provider-local bet model or a post-commit UPDATE race.
    pub fn place_investment_bet_atomic(
        &self,
        request: PlaceBetRequest,
        investment_target_id: i64,
        investment_direction: &str,
    ) -> Result<PendingBetRecord, BettingServiceRepositoryError> {
        self.place_bet_atomic_with_attribution(
            request,
            Some(BetAttribution {
                investment_target_id: Some(investment_target_id),
                investment_direction: Some(investment_direction.to_owned()),
            }),
        )
    }

    /// Place a complete automatic-bet batch against one balance snapshot.
    /// Domain-invalid candidates roll back to their own savepoint while the
    /// rest of the batch remains atomic and preserves placement order.
    pub fn place_automatic_bets_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: Option<i64>,
        bet_time: i64,
        requests: &[AutomaticBetRequest],
    ) -> Result<AutomaticBetOutcome, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload = pending_match_id
            .map(|id| {
                transaction
                    .query_row(
                        "SELECT payload FROM pending_matches
                         WHERE pending_match_id = ?1 AND guild_id = ?2",
                        params![id, guild_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or(BettingServiceRepositoryError::MissingPendingMatch {
                        guild_id,
                        pending_match_id: id,
                    })
            })
            .transpose()?;

        let mut balance_snapshot = BTreeMap::new();
        for request in requests {
            if balance_snapshot.contains_key(&request.discord_id) {
                continue;
            }
            let balance = transaction
                .query_row(
                    "SELECT COALESCE(jopacoin_balance, 0) FROM players
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![request.discord_id, guild_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(balance) = balance {
                balance_snapshot.insert(request.discord_id, balance);
            }
        }

        let mut outcome = AutomaticBetOutcome {
            created: Vec::new(),
            skipped: Vec::new(),
            balance_snapshot,
        };
        for request in requests {
            transaction.execute_batch("SAVEPOINT automatic_bet_candidate")?;
            let result = automatic_candidate(
                &transaction,
                guild_id,
                pending_match_id,
                payload.as_deref(),
                bet_time,
                *request,
                &outcome.balance_snapshot,
            );
            match result {
                Ok(record) => {
                    transaction.execute_batch("RELEASE SAVEPOINT automatic_bet_candidate")?;
                    outcome.created.push(record);
                }
                Err(error) if is_candidate_skip(&error) => {
                    transaction.execute_batch(
                        "ROLLBACK TO SAVEPOINT automatic_bet_candidate;
                         RELEASE SAVEPOINT automatic_bet_candidate",
                    )?;
                    outcome.skipped.push(request.discord_id);
                }
                Err(error) => return Err(error),
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    /// Place exact automatic wagers from one locked balance snapshot.
    ///
    /// This is the production spectator-wager seam: unlike the historical
    /// integer-percent API, the amount is not truncated to whole percentage
    /// points.  Requests are idempotent for a pending match, and every
    /// candidate is isolated by a savepoint so a stale balance or debt limit
    /// rejection does not suppress the remaining tier.
    pub fn place_automatic_amount_bets_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: Option<i64>,
        bet_time: i64,
        requests: &[AutomaticAmountBetRequest],
        max_debt: i64,
    ) -> Result<AutomaticBetOutcome, BettingServiceRepositoryError> {
        let candidates = requests
            .iter()
            .map(|request| ExactAutomaticCandidate {
                discord_id: request.discord_id,
                team: request.team,
                amount: Some(request.amount),
                basis_points: None,
            })
            .collect::<Vec<_>>();
        self.place_exact_automatic_bets_atomic(
            guild_id,
            pending_match_id,
            bet_time,
            &candidates,
            max_debt,
        )
    }

    /// Place automatic wagers sized from exact basis points of the balance
    /// snapshot taken by the same SQLite transaction.  Basis-point rounding
    /// matches Python's `round(balance * fraction)` ties-to-even behavior.
    pub fn place_automatic_basis_point_bets_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: Option<i64>,
        bet_time: i64,
        requests: &[AutomaticBasisPointBetRequest],
        max_debt: i64,
    ) -> Result<AutomaticBetOutcome, BettingServiceRepositoryError> {
        let candidates = requests
            .iter()
            .map(|request| ExactAutomaticCandidate {
                discord_id: request.discord_id,
                team: request.team,
                amount: None,
                basis_points: Some(request.basis_points),
            })
            .collect::<Vec<_>>();
        self.place_exact_automatic_bets_atomic(
            guild_id,
            pending_match_id,
            bet_time,
            &candidates,
            max_debt,
        )
    }

    fn place_exact_automatic_bets_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: Option<i64>,
        bet_time: i64,
        candidates: &[ExactAutomaticCandidate],
        max_debt: i64,
    ) -> Result<AutomaticBetOutcome, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload = pending_match_id
            .map(|id| {
                transaction
                    .query_row(
                        "SELECT payload FROM pending_matches
                         WHERE pending_match_id = ?1 AND guild_id = ?2",
                        params![id, guild_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or(BettingServiceRepositoryError::MissingPendingMatch {
                        guild_id,
                        pending_match_id: id,
                    })
            })
            .transpose()?;
        let balance_snapshot = balance_snapshot(
            &transaction,
            guild_id,
            &candidates
                .iter()
                .map(|candidate| candidate.discord_id)
                .collect::<Vec<_>>(),
        )?;
        let mut outcome = AutomaticBetOutcome {
            created: Vec::new(),
            skipped: Vec::new(),
            balance_snapshot,
        };

        for candidate in candidates {
            transaction.execute_batch("SAVEPOINT exact_automatic_bet_candidate")?;
            let result = exact_automatic_candidate(ExactAutomaticContext {
                transaction: &transaction,
                guild_id,
                pending_match_id,
                payload: payload.as_deref(),
                bet_time,
                candidate: *candidate,
                max_debt,
                balances: &outcome.balance_snapshot,
            });
            match result {
                Ok(record) => {
                    transaction.execute_batch("RELEASE SAVEPOINT exact_automatic_bet_candidate")?;
                    outcome.created.push(record);
                }
                Err(error) if is_candidate_skip(&error) => {
                    transaction.execute_batch(
                        "ROLLBACK TO SAVEPOINT exact_automatic_bet_candidate;
                         RELEASE SAVEPOINT exact_automatic_bet_candidate",
                    )?;
                    outcome.skipped.push(candidate.discord_id);
                }
                Err(error) => return Err(error),
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    /// Create the configured long/short positions for a freshly shuffled
    /// match.  This is the narrow match-provider hook for Python's
    /// `create_investment_bets`: positions, shuffle-start balances, team
    /// restrictions, odds attribution, wallet debits, and the attributed
    /// `bets` rows all share one `BEGIN IMMEDIATE` transaction.
    pub fn create_configured_investment_bets_for_shuffle(
        &self,
        request: ConfiguredInvestmentRequest,
    ) -> Result<AutomaticBetOutcome, BettingServiceRepositoryError> {
        self.create_configured_investment_bets_atomic_with_starting_balances(
            ConfiguredInvestmentBatch {
                guild_id: request.guild_id,
                pending_match_id: request.pending_match_id,
                bet_time: request.bet_time,
                radiant_ids: &request.radiant_ids,
                dire_ids: &request.dire_ids,
                max_debt: request.max_debt,
                starting_balances: Some(&request.starting_balances),
            },
        )
    }

    pub fn create_configured_investment_bets_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: Option<i64>,
        bet_time: i64,
        radiant_ids: &[i64],
        dire_ids: &[i64],
        max_debt: i64,
    ) -> Result<AutomaticBetOutcome, BettingServiceRepositoryError> {
        self.create_configured_investment_bets_atomic_with_starting_balances(
            ConfiguredInvestmentBatch {
                guild_id,
                pending_match_id,
                bet_time,
                radiant_ids,
                dire_ids,
                max_debt,
                starting_balances: None,
            },
        )
    }

    fn create_configured_investment_bets_atomic_with_starting_balances(
        &self,
        batch: ConfiguredInvestmentBatch<'_>,
    ) -> Result<AutomaticBetOutcome, BettingServiceRepositoryError> {
        let ConfiguredInvestmentBatch {
            guild_id: requested_guild_id,
            pending_match_id,
            bet_time,
            radiant_ids,
            dire_ids,
            max_debt,
            starting_balances,
        } = batch;
        let guild_id = Self::normalize_guild_id(requested_guild_id);
        let target_ids = radiant_ids
            .iter()
            .chain(dire_ids.iter())
            .copied()
            .collect::<BTreeSet<_>>();
        if target_ids.is_empty() {
            return Ok(AutomaticBetOutcome {
                created: Vec::new(),
                skipped: Vec::new(),
                balance_snapshot: BTreeMap::new(),
            });
        }
        let mut team_by_player = BTreeMap::new();
        for discord_id in radiant_ids {
            team_by_player.insert(*discord_id, BettingTeam::Radiant);
        }
        for discord_id in dire_ids {
            team_by_player
                .entry(*discord_id)
                .or_insert(BettingTeam::Dire);
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let positions = configured_investments_in_transaction(&transaction, guild_id, &target_ids)?;
        let balance_snapshot = balance_snapshot(
            &transaction,
            guild_id,
            &positions
                .iter()
                .map(|position| position.investor_id)
                .collect::<Vec<_>>(),
        )?;
        let payload = pending_match_id
            .map(|id| {
                transaction
                    .query_row(
                        "SELECT payload FROM pending_matches
                         WHERE pending_match_id = ?1 AND guild_id = ?2",
                        params![id, guild_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or(BettingServiceRepositoryError::MissingPendingMatch {
                        guild_id,
                        pending_match_id: id,
                    })
            })
            .transpose()?;
        let mut totals =
            pending_match_totals_in_transaction(&transaction, guild_id, pending_match_id)?;
        let mut outcome = AutomaticBetOutcome {
            created: Vec::new(),
            skipped: Vec::new(),
            balance_snapshot,
        };
        for position in positions {
            let Some(starting_balance) = starting_balances
                .and_then(|balances| balances.get(&position.investor_id).copied())
                .or_else(|| outcome.balance_snapshot.get(&position.investor_id).copied())
            else {
                outcome.skipped.push(position.investor_id);
                continue;
            };
            if starting_balance < 50 {
                outcome.skipped.push(position.investor_id);
                continue;
            }
            let Some(balance) = outcome.balance_snapshot.get(&position.investor_id).copied() else {
                outcome.skipped.push(position.investor_id);
                continue;
            };
            if balance <= 0 {
                outcome.skipped.push(position.investor_id);
                continue;
            }
            let Some(target_team) = team_by_player.get(&position.target_id).copied() else {
                continue;
            };
            let team = if position.direction == "long" {
                target_team
            } else {
                match target_team {
                    BettingTeam::Radiant => BettingTeam::Dire,
                    BettingTeam::Dire => BettingTeam::Radiant,
                }
            };
            if team_by_player
                .get(&position.investor_id)
                .is_some_and(|investor_team| *investor_team != team)
            {
                outcome.skipped.push(position.investor_id);
                continue;
            }
            let amount = balance
                .checked_mul(position.percentage)
                .ok_or(BettingServiceRepositoryError::WagerOverflow)?
                / 100;
            if amount < 1 {
                outcome.skipped.push(position.investor_id);
                continue;
            }
            let total = totals.total() as f64;
            let side = match team {
                BettingTeam::Radiant => totals.radiant,
                BettingTeam::Dire => totals.dire,
            } as f64;
            let request = PlaceBetRequest {
                guild_id: Some(guild_id),
                pending_match_id: pending_match_id.unwrap_or_default(),
                discord_id: position.investor_id,
                team,
                amount,
                bet_time,
                leverage: 1,
                max_debt,
                is_blind: true,
                odds_at_placement: (total > 0.0 && side > 0.0).then_some(total / side),
            };
            transaction.execute_batch("SAVEPOINT configured_investment")?;
            let result = payload
                .as_deref()
                .map_or(Ok(()), |payload| {
                    validate_pending_bet(&transaction, payload, request)
                })
                .and_then(|()| {
                    insert_bet_in_transaction_with_attribution(
                        &transaction,
                        guild_id,
                        request,
                        true,
                        false,
                        Some((position.target_id, &position.direction)),
                    )
                });
            match result {
                Ok(record) => {
                    transaction.execute_batch("RELEASE SAVEPOINT configured_investment")?;
                    match team {
                        BettingTeam::Radiant => {
                            totals.radiant = totals.radiant.saturating_add(amount)
                        }
                        BettingTeam::Dire => totals.dire = totals.dire.saturating_add(amount),
                    }
                    outcome.created.push(record);
                }
                Err(error) if is_candidate_skip(&error) => {
                    transaction.execute_batch(
                        "ROLLBACK TO SAVEPOINT configured_investment;
                         RELEASE SAVEPOINT configured_investment",
                    )?;
                    outcome.skipped.push(position.investor_id);
                }
                Err(error) => return Err(error),
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    /// Plan and place the complete automatic blind-bet batch from one locked
    /// balance snapshot. Normal blind bets enforce the configured threshold;
    /// bomb-pot bets add the mandatory ante and may cross zero down to the
    /// maximum debt. Each invalid candidate rolls back to its own savepoint so
    /// one bad player never suppresses the remaining mandatory antes.
    pub fn create_auto_blind_bets_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: Option<i64>,
        bet_time: i64,
        candidates: &[BlindBetCandidate],
        policy: BlindBetPolicy,
    ) -> Result<BlindBetOutcome, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload = pending_match_id
            .map(|id| {
                transaction
                    .query_row(
                        "SELECT payload FROM pending_matches
                         WHERE pending_match_id = ?1 AND guild_id = ?2",
                        params![id, guild_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or(BettingServiceRepositoryError::MissingPendingMatch {
                        guild_id,
                        pending_match_id: id,
                    })
            })
            .transpose()?;
        let balance_snapshot = balance_snapshot(
            &transaction,
            guild_id,
            &candidates
                .iter()
                .map(|candidate| candidate.discord_id)
                .collect::<Vec<_>>(),
        )?;
        let mut outcome = BlindBetOutcome {
            created: Vec::new(),
            skipped: Vec::new(),
            balance_snapshot,
            total_radiant: 0,
            total_dire: 0,
            is_bomb_pot: policy.is_bomb_pot,
        };

        for candidate in candidates {
            let Some(balance) = outcome.balance_snapshot.get(&candidate.discord_id).copied() else {
                outcome.skipped.push(BlindBetSkip {
                    discord_id: candidate.discord_id,
                    reason: "player not found".to_owned(),
                });
                continue;
            };
            let amount = if policy.is_bomb_pot {
                let ante = candidate.ante_override.unwrap_or(policy.bomb_ante).max(0);
                rounded_percentage(balance.max(0), policy.bomb_percentage)?
                    .checked_add(ante)
                    .ok_or(BettingServiceRepositoryError::WagerOverflow)?
                    .max(ante)
            } else {
                if balance < policy.normal_threshold {
                    outcome.skipped.push(BlindBetSkip {
                        discord_id: candidate.discord_id,
                        reason: format!(
                            "balance {balance} < threshold {}",
                            policy.normal_threshold
                        ),
                    });
                    continue;
                }
                rounded_percentage(balance, policy.normal_percentage)?
            };
            if amount < 1 {
                outcome.skipped.push(BlindBetSkip {
                    discord_id: candidate.discord_id,
                    reason: format!("blind amount {amount} < 1"),
                });
                continue;
            }
            let request = PlaceBetRequest {
                guild_id: Some(guild_id),
                pending_match_id: pending_match_id.unwrap_or_default(),
                discord_id: candidate.discord_id,
                team: candidate.team,
                amount,
                bet_time,
                leverage: 1,
                max_debt: policy.max_debt,
                is_blind: true,
                odds_at_placement: None,
            };
            transaction.execute_batch("SAVEPOINT blind_bet_candidate")?;
            let result = payload
                .as_deref()
                .map_or(Ok(()), |payload| {
                    validate_pending_bet(&transaction, payload, request)
                })
                .and_then(|()| {
                    insert_bet_in_transaction(
                        &transaction,
                        guild_id,
                        request,
                        true,
                        policy.is_bomb_pot,
                    )
                });
            match result {
                Ok(record) => {
                    transaction.execute_batch("RELEASE SAVEPOINT blind_bet_candidate")?;
                    match record.team {
                        BettingTeam::Radiant => {
                            outcome.total_radiant = outcome.total_radiant.saturating_add(amount);
                        }
                        BettingTeam::Dire => {
                            outcome.total_dire = outcome.total_dire.saturating_add(amount);
                        }
                    }
                    outcome.created.push(record);
                }
                Err(error) if is_candidate_skip(&error) => {
                    transaction.execute_batch(
                        "ROLLBACK TO SAVEPOINT blind_bet_candidate;
                         RELEASE SAVEPOINT blind_bet_candidate",
                    )?;
                    outcome.skipped.push(BlindBetSkip {
                        discord_id: candidate.discord_id,
                        reason: error.to_string(),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    pub fn get_pending_bets(
        &self,
        guild_id: Option<i64>,
        discord_id: Option<i64>,
        since_ts: i64,
        pending_match_id: Option<i64>,
    ) -> Result<Vec<PendingBetRecord>, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut records = Vec::new();
        let sql = match (discord_id, pending_match_id) {
            (Some(_), Some(_)) => {
                "SELECT bet_id, guild_id, discord_id, team_bet_on, amount,
                        bet_time, COALESCE(leverage, 1), COALESCE(is_blind, 0),
                        odds_at_placement, pending_match_id,
                        investment_target_id, investment_direction
                 FROM bets WHERE guild_id = ?1 AND discord_id = ?2
                   AND match_id IS NULL
                   AND (pending_match_id = ?3 OR
                        (pending_match_id IS NULL AND bet_time >= ?4))
                 ORDER BY bet_time, bet_id"
            }
            (Some(_), None) => {
                "SELECT bet_id, guild_id, discord_id, team_bet_on, amount,
                        bet_time, COALESCE(leverage, 1), COALESCE(is_blind, 0),
                        odds_at_placement, pending_match_id,
                        investment_target_id, investment_direction
                 FROM bets WHERE guild_id = ?1 AND discord_id = ?2
                   AND match_id IS NULL AND bet_time >= ?3
                 ORDER BY bet_time, bet_id"
            }
            (None, Some(_)) => {
                "SELECT bet_id, guild_id, discord_id, team_bet_on, amount,
                        bet_time, COALESCE(leverage, 1), COALESCE(is_blind, 0),
                        odds_at_placement, pending_match_id,
                        investment_target_id, investment_direction
                 FROM bets WHERE guild_id = ?1 AND match_id IS NULL
                   AND (pending_match_id = ?2 OR
                        (pending_match_id IS NULL AND bet_time >= ?3))
                 ORDER BY bet_time, bet_id"
            }
            (None, None) => {
                "SELECT bet_id, guild_id, discord_id, team_bet_on, amount,
                        bet_time, COALESCE(leverage, 1), COALESCE(is_blind, 0),
                        odds_at_placement, pending_match_id,
                        investment_target_id, investment_direction
                 FROM bets WHERE guild_id = ?1 AND match_id IS NULL
                   AND bet_time >= ?2 ORDER BY bet_time, bet_id"
            }
        };
        let mut statement = connection.prepare(sql)?;
        match (discord_id, pending_match_id) {
            (Some(discord_id), Some(pending_match_id)) => {
                let rows = statement.query_map(
                    params![guild_id, discord_id, pending_match_id, since_ts],
                    pending_bet_from_row,
                )?;
                records.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            (Some(discord_id), None) => {
                let rows = statement.query_map(
                    params![guild_id, discord_id, since_ts],
                    pending_bet_from_row,
                )?;
                records.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            (None, Some(pending_match_id)) => {
                let rows = statement.query_map(
                    params![guild_id, pending_match_id, since_ts],
                    pending_bet_from_row,
                )?;
                records.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            (None, None) => {
                let rows =
                    statement.query_map(params![guild_id, since_ts], pending_bet_from_row)?;
                records.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
        }
        Ok(records)
    }

    /// Return the leverage attached to each settled bet for a recorded match.
    ///
    /// Match recovery runs after settlement has moved rows out of the pending
    /// state.  Keeping this read-only seam on the betting repository lets the
    /// runtime rebuild the exact post-settlement Neon observer request,
    /// including per-bet leverage, without duplicating betting SQL or adding
    /// a new schema table.
    pub fn get_match_bet_leverages(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT bet_id, COALESCE(leverage, 1)
             FROM bets
             WHERE match_id = ?1 AND guild_id = ?2
             ORDER BY bet_id",
        )?;
        let mut result = BTreeMap::new();
        for row in statement.query_map(params![match_id, guild_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?.max(1)))
        })? {
            let (bet_id, leverage) = row?;
            result.insert(bet_id, leverage);
        }
        Ok(result)
    }

    pub fn get_pending_totals(
        &self,
        guild_id: Option<i64>,
        since_ts: i64,
        pending_match_id: Option<i64>,
    ) -> Result<PotTotals, BettingServiceRepositoryError> {
        let bets = self.get_pending_bets(guild_id, None, since_ts, pending_match_id)?;
        let mut totals = PotTotals::default();
        for bet in bets {
            match bet.team {
                BettingTeam::Radiant => {
                    totals.radiant = totals.radiant.saturating_add(bet.effective_amount());
                }
                BettingTeam::Dire => {
                    totals.dire = totals.dire.saturating_add(bet.effective_amount());
                }
            }
        }
        Ok(totals)
    }

    pub fn refund_pending_bets_atomic(
        &self,
        guild_id: Option<i64>,
        since_ts: i64,
        pending_match_id: Option<i64>,
    ) -> Result<usize, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let bets =
            pending_bets_in_transaction(&transaction, guild_id, None, since_ts, pending_match_id)?;
        set_ledger_context(
            &transaction,
            "bet_refund",
            pending_match_id,
            "pending bets refunded",
        )?;
        for bet in &bets {
            transaction.execute(
                "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                                    updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![bet.effective_amount(), bet.discord_id, guild_id],
            )?;
        }
        clear_ledger_context(&transaction)?;
        delete_pending_rows(&transaction, guild_id, since_ts, pending_match_id)?;
        transaction.commit()?;
        Ok(bets.len())
    }

    /// Settle untagged rows exactly once. Seed reservation/return is owned by
    /// `dota_bet_seed_repository`; the optional seed split is intentionally
    /// absent here so monetary stock cannot be consumed twice.
    pub fn settle_pending_bets_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        since_ts: i64,
        pending_match_id: Option<i64>,
        winning_team: BettingTeam,
        mode: BettingMode,
    ) -> Result<SettlementResult, BettingServiceRepositoryError> {
        self.settle_pending_bets_with_adjustments_atomic(
            match_id,
            guild_id,
            since_ts,
            pending_match_id,
            winning_team,
            mode,
            &BetSettlementAdjustments::default(),
        )
    }

    /// Settle gross payouts while netting bankruptcy from positive aggregate
    /// profit and auditing vanity tax as a separate debit in the same SQLite
    /// transaction. Stakes are never part of either deduction basis.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_pending_bets_with_adjustments_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        since_ts: i64,
        pending_match_id: Option<i64>,
        winning_team: BettingTeam,
        mode: BettingMode,
        adjustments: &BetSettlementAdjustments,
    ) -> Result<SettlementResult, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let bets =
            pending_bets_in_transaction(&transaction, guild_id, None, since_ts, pending_match_id)?;
        if bets.is_empty() {
            let seed = if adjustments.consume_seed {
                pending_match_id.map_or_else(
                    || Ok(SeedSettlement::default()),
                    |pending_match_id| {
                        settle_seed_in_transaction(
                            &transaction,
                            guild_id,
                            pending_match_id,
                            mode,
                            None,
                        )
                    },
                )?
            } else {
                SeedSettlement::default()
            };
            transaction.commit()?;
            return Ok(SettlementResult {
                seed,
                ..SettlementResult::default()
            });
        }

        let total_pool = bets
            .iter()
            .map(PendingBetRecord::effective_amount)
            .sum::<i64>();
        let winning_pool = bets
            .iter()
            .filter(|bet| bet.team == winning_team)
            .map(PendingBetRecord::effective_amount)
            .sum::<i64>();
        let seed = if adjustments.consume_seed {
            pending_match_id.map_or_else(
                || Ok(SeedSettlement::default()),
                |pending_match_id| {
                    settle_seed_in_transaction(
                        &transaction,
                        guild_id,
                        pending_match_id,
                        mode,
                        (winning_pool > 0).then_some(winning_team),
                    )
                },
            )?
        } else {
            SeedSettlement::default()
        };
        let payouts = calculate_payouts(PayoutCalculation {
            bets: &bets,
            winning_team,
            mode,
            total_pool,
            winning_pool,
            winner_seed: seed.winner_seed,
            house_payout_multiplier: normalized_house_multiplier(
                adjustments.house_payout_multiplier,
            ),
            payout_multiplier: normalized_payout_multiplier(adjustments.payout_multiplier),
        });
        let mut credits = BTreeMap::<i64, i64>::new();
        for (bet_id, payout) in &payouts {
            if *payout > 0 {
                let discord_id = bets
                    .iter()
                    .find(|bet| bet.bet_id == *bet_id)
                    .map(|bet| bet.discord_id)
                    .unwrap_or_default();
                credits
                    .entry(discord_id)
                    .and_modify(|credit| *credit = credit.saturating_add(*payout))
                    .or_insert(*payout);
            }
        }

        let mut profit_inputs = BTreeMap::<i64, (i64, i64)>::new();
        for bet in &bets {
            if let Some(payout) = payouts.get(&bet.bet_id) {
                let aggregate = profit_inputs.entry(bet.discord_id).or_default();
                aggregate.0 = aggregate.0.saturating_add(*payout);
                aggregate.1 = aggregate.1.saturating_add(bet.effective_amount());
            }
        }
        for bet in &bets {
            if !payouts.contains_key(&bet.bet_id)
                && let Some(aggregate) = profit_inputs.get_mut(&bet.discord_id)
            {
                aggregate.1 = aggregate.1.saturating_add(bet.effective_amount());
            }
        }

        let mut bankruptcy_penalties = BTreeMap::new();
        let mut vanity_taxes = BTreeMap::new();
        let mut low_priority_taxes = BTreeMap::new();
        for (discord_id, (payout, stake)) in profit_inputs {
            let profit = payout.saturating_sub(stake);
            if profit <= 0 {
                continue;
            }
            let kept_rate = adjustments.bankruptcy_kept_rate.or_else(|| {
                adjustments
                    .bankruptcy_keep_basis_points
                    .map(|basis_points| basis_points.clamp(0, 10_000) as f64 / 10_000.0)
            });
            if let Some(kept_rate) = kept_rate {
                let penalty_games = transaction
                    .query_row(
                        "SELECT COALESCE(penalty_games_remaining, 0)
                         FROM bankruptcy_state
                         WHERE discord_id = ?1 AND guild_id = ?2",
                        params![discord_id, guild_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or_default();
                if penalty_games > 0 {
                    let penalty = (profit as f64 * (1.0 - normalized_rate(kept_rate, 1.0))) as i64;
                    if penalty > 0 {
                        if let Some(credit) = credits.get_mut(&discord_id) {
                            *credit = credit.saturating_sub(penalty);
                        }
                        bankruptcy_penalties.insert(discord_id, penalty);
                    }
                }
            }
            let vanity_tax = if adjustments.vanity_taxable_ids.contains(&discord_id) {
                let vanity_rate = adjustments.vanity_tax_rate.map_or_else(
                    || adjustments.vanity_tax_basis_points.clamp(0, 10_000) as f64 / 10_000.0,
                    |rate| normalized_rate(rate, 1.0),
                );
                let vanity_tax = (profit as f64 * vanity_rate) as i64;
                if vanity_tax > 0 {
                    vanity_taxes.insert(discord_id, vanity_tax);
                }
                vanity_tax
            } else {
                0
            };
            if adjustments.low_priority_taxable_ids.contains(&discord_id) {
                // Low priority taxes the same aggregate profit as vanity, not
                // the post-vanity remainder, so both rates stay additive.
                let low_priority_rate = adjustments
                    .low_priority_tax_rate
                    .map_or(0.0, |rate| normalized_rate(rate, 1.0));
                // The bankruptcy withholding and both taxes are independent
                // fractions of one basis, so their sum can exceed the profit
                // they are withheld from. Clamp the combined withholding to
                // `profit` by trimming the low-priority share, which keeps
                // already-reported bankruptcy and vanity totals exact.
                let headroom = profit
                    .saturating_sub(
                        bankruptcy_penalties
                            .get(&discord_id)
                            .copied()
                            .unwrap_or_default(),
                    )
                    .saturating_sub(vanity_tax)
                    .max(0);
                let low_priority_tax = ((profit as f64 * low_priority_rate) as i64).min(headroom);
                if low_priority_tax > 0 {
                    low_priority_taxes.insert(discord_id, low_priority_tax);
                }
            }
        }

        if adjustments.vanity_tax_rate.is_some()
            || adjustments.vanity_tax_basis_points > 0
            || !adjustments.vanity_taxable_ids.is_empty()
            || adjustments.low_priority_tax_rate.is_some()
            || !adjustments.low_priority_taxable_ids.is_empty()
        {
            transaction.execute(
                "DELETE FROM bet_settlement_taxes
                 WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, guild_id],
            )?;
            // The row is keyed by player, and either withholding can apply on
            // its own, so persist the union of both taxed sets.
            let taxed_ids = vanity_taxes
                .keys()
                .chain(low_priority_taxes.keys())
                .copied()
                .collect::<BTreeSet<i64>>();
            for discord_id in taxed_ids {
                let vanity_tax = vanity_taxes.get(&discord_id).copied().unwrap_or_default();
                let low_priority_tax = low_priority_taxes
                    .get(&discord_id)
                    .copied()
                    .unwrap_or_default();
                transaction.execute(
                    "INSERT INTO bet_settlement_taxes
                         (match_id, guild_id, discord_id, vanity_tax, low_priority_tax)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![match_id, guild_id, discord_id, vanity_tax, low_priority_tax],
                )?;
            }
        }
        set_ledger_context(
            &transaction,
            "bet_settlement",
            pending_match_id,
            "pending bet payout",
        )?;
        for (discord_id, amount) in &credits {
            transaction.execute(
                "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                                    updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![amount, discord_id, guild_id],
            )?;
        }
        clear_ledger_context(&transaction)?;
        if !vanity_taxes.is_empty() {
            set_ledger_context(
                &transaction,
                "vanity_tax",
                pending_match_id,
                "vanity tax on JC profit",
            )?;
            for (discord_id, vanity_tax) in &vanity_taxes {
                transaction.execute(
                    "UPDATE players
                     SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?2 AND guild_id = ?3",
                    params![vanity_tax, discord_id, guild_id],
                )?;
            }
            clear_ledger_context(&transaction)?;
        }
        if !low_priority_taxes.is_empty() {
            set_ledger_context(
                &transaction,
                "low_priority_tax",
                pending_match_id,
                "low priority tax on JC profit",
            )?;
            for (discord_id, low_priority_tax) in &low_priority_taxes {
                transaction.execute(
                    "UPDATE players
                     SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?2 AND guild_id = ?3",
                    params![low_priority_tax, discord_id, guild_id],
                )?;
            }
            clear_ledger_context(&transaction)?;
        }
        for bet in &bets {
            if let Some(payout) = payouts.get(&bet.bet_id) {
                transaction.execute(
                    "UPDATE bets SET payout = ?1, match_id = ?2
                     WHERE bet_id = ?3 AND match_id IS NULL",
                    params![payout, match_id, bet.bet_id],
                )?;
            } else {
                // Losing rows keep a NULL payout, but must still be tagged in
                // the same transaction or a retry can settle them again.
                transaction.execute(
                    "UPDATE bets SET match_id = ?1
                     WHERE bet_id = ?2 AND match_id IS NULL",
                    params![match_id, bet.bet_id],
                )?;
            }
        }
        let mut result = SettlementResult {
            bankruptcy_penalties,
            vanity_taxes,
            low_priority_taxes,
            seed,
            ..SettlementResult::default()
        };
        for bet in bets {
            let payout = payouts.get(&bet.bet_id).copied().unwrap_or_default();
            let balance_after = transaction.query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![bet.discord_id, guild_id],
                |row| row.get(0),
            )?;
            let settled = SettledBet {
                bet_id: bet.bet_id,
                discord_id: bet.discord_id,
                team: bet.team,
                effective_amount: bet.effective_amount(),
                payout,
                balance_after,
            };
            if bet.team == winning_team {
                result.winners.push(settled);
            } else {
                result.losers.push(settled);
            }
        }
        transaction.commit()?;
        Ok(result)
    }

    pub fn consume_and_credit_atomic(
        &self,
        buff_id: i64,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<bool, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let claimed = transaction.execute(
            "UPDATE manashop_buffs SET triggered = 1 WHERE id = ?1 AND triggered = 0",
            [buff_id],
        )?;
        if claimed == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        set_ledger_context(
            &transaction,
            "manashop_buff",
            Some(buff_id),
            "one-shot blessing credit",
        )?;
        let changed = transaction.execute(
            "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                                updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, discord_id, guild_id],
        )?;
        if changed == 0 {
            return Err(BettingServiceRepositoryError::MissingPlayer {
                discord_id,
                guild_id,
            });
        }
        clear_ledger_context(&transaction)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Atomically consume Communion and attribute its credit to the same
    /// durable match-win tuple as the base reward. A correction retry can then
    /// reconstruct the exact pre-Blood-Pact balance delta by summing ledger
    /// rows without consuming or paying the blessing twice.
    pub fn consume_and_credit_match_win_atomic(
        &self,
        buff_id: i64,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        match_id: i64,
    ) -> Result<bool, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let claimed = transaction.execute(
            "UPDATE manashop_buffs SET triggered=1 WHERE id=?1 AND triggered=0",
            [buff_id],
        )?;
        if claimed == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
        transaction.execute(
            "INSERT INTO economy_ledger_context
                 (id,source,related_type,related_id,reason,metadata)
             VALUES (1,'manashop_buff','match_win_bonus',?1,
                     'Communion match-win bonus',?2)",
            params![
                match_id.to_string(),
                format!("{{\"communion_bonus\":{amount},\"buff_id\":{buff_id}}}")
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![amount, discord_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        if changed != 1 {
            return Err(BettingServiceRepositoryError::MissingPlayer {
                discord_id,
                guild_id,
            });
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Recover durable Blood Pact debits attributable to one bet settlement.
    /// Both the legacy direct-transfer ledger path and the protection-aware
    /// hostile-loss path are included, matching Python retry accounting.
    pub fn match_blood_pact_skims(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut result = BTreeMap::new();
        let mut direct = connection.prepare(
            "SELECT account_id,-SUM(delta)
             FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_type='player'
               AND source='dota_bet_blood_pact'
               AND related_type='match' AND related_id=?2 AND delta<0
             GROUP BY account_id",
        )?;
        for row in direct.query_map(params![guild_id, match_id.to_string()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (discord_id, skimmed) = row?;
            result.insert(discord_id, skimmed);
        }
        let mut protected = connection.prepare(
            "SELECT victim_id,SUM(applied)
             FROM hostile_loss_events
             WHERE guild_id=?1 AND kind='blood_pact'
               AND event_key LIKE ?2 AND applied>0
             GROUP BY victim_id",
        )?;
        let pattern = format!("dota-bet-blood-pact:{match_id}:%");
        for row in protected.query_map(params![guild_id, pattern], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (discord_id, skimmed) = row?;
            result
                .entry(discord_id)
                .and_modify(|amount| *amount = amount.saturating_add(skimmed))
                .or_insert(skimmed);
        }
        Ok(result)
    }

    /// Recover the durable settlement snapshot for a recorded match.
    ///
    /// Bet settlement commits before Match writes its aggregate `jc_changes`
    /// snapshot.  A later snapshot failure therefore leaves no pending rows
    /// for `settle_pending_bets_with_adjustments_atomic` to return on retry.
    /// This read-only seam reconstructs the same winner/loser distribution
    /// from the already tagged `bets` rows, including the persisted vanity-tax
    /// and netted bankruptcy components.  Blood Pact debits are recovered by
    /// [`Self::match_blood_pact_skims`] because protection-aware debits live in
    /// their own durable event tables.
    pub fn recover_settlement_for_match(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<SettlementResult, BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let Some((winning_team, pending_match_id)) = connection
            .query_row(
                "SELECT winning_team, pending_match_id
                 FROM matches WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
        else {
            return Err(BettingServiceRepositoryError::MissingMatch { match_id, guild_id });
        };

        let mut statement = connection.prepare(
            "SELECT b.bet_id, b.discord_id, b.team_bet_on,
                    b.amount * COALESCE(b.leverage, 1),
                    COALESCE(b.payout, 0),
                    COALESCE(p.jopacoin_balance, 0)
             FROM bets b
             LEFT JOIN players p
               ON p.discord_id = b.discord_id AND p.guild_id = b.guild_id
             WHERE b.match_id = ?1 AND b.guild_id = ?2
             ORDER BY b.bet_time, b.bet_id",
        )?;
        let mut winners = Vec::new();
        let mut losers = Vec::new();
        let mut rows = statement.query(params![match_id, guild_id])?;
        while let Some(row) = rows.next()? {
            let team = parse_team(&row.get::<_, String>(2)?)?;
            let settled = SettledBet {
                bet_id: row.get(0)?,
                discord_id: row.get(1)?,
                team,
                effective_amount: row.get(3)?,
                payout: row.get(4)?,
                balance_after: row.get(5)?,
            };
            if (team == BettingTeam::Radiant && winning_team == 1)
                || (team == BettingTeam::Dire && winning_team == 2)
            {
                winners.push(settled);
            } else {
                losers.push(settled);
            }
        }

        let mut vanity_taxes = BTreeMap::new();
        let mut low_priority_taxes = BTreeMap::new();
        let mut tax_statement = connection.prepare(
            "SELECT discord_id, vanity_tax, low_priority_tax
             FROM bet_settlement_taxes
             WHERE match_id = ?1 AND guild_id = ?2",
        )?;
        let mut tax_rows = tax_statement.query(params![match_id, guild_id])?;
        while let Some(row) = tax_rows.next()? {
            let discord_id: i64 = row.get(0)?;
            let vanity_tax: i64 = row.get(1)?;
            let low_priority_tax: i64 = row.get(2)?;
            if vanity_tax > 0 {
                vanity_taxes.insert(discord_id, vanity_tax);
            }
            if low_priority_tax > 0 {
                low_priority_taxes.insert(discord_id, low_priority_tax);
            }
        }

        // The settlement transaction records each player's payout after the
        // bankruptcy withholding in the immutable ledger.  Vanity and
        // low-priority tax are separate ledger sources, so gross -
        // bet-settlement credit recovers the bankruptcy withholding without
        // introducing a second settlement table or a schema fork.
        let mut net_credits = BTreeMap::<i64, i64>::new();
        if let Some(pending_match_id) = pending_match_id {
            let mut ledger_statement = connection.prepare(
                "SELECT account_id, COALESCE(SUM(delta), 0)
                 FROM economy_ledger_entries
                 WHERE guild_id = ?1 AND account_type = 'player'
                   AND source = 'bet_settlement'
                   AND related_type = 'pending_match'
                   AND related_id = ?2
                 GROUP BY account_id",
            )?;
            let mut ledger_rows =
                ledger_statement.query(params![guild_id, pending_match_id.to_string()])?;
            while let Some(row) = ledger_rows.next()? {
                net_credits.insert(row.get(0)?, row.get(1)?);
            }
        }
        let mut gross_by_player = BTreeMap::new();
        for bet in &winners {
            gross_by_player
                .entry(bet.discord_id)
                .and_modify(|gross: &mut i64| *gross = gross.saturating_add(bet.payout))
                .or_insert(bet.payout);
        }
        let mut bankruptcy_penalties = BTreeMap::new();
        for (discord_id, gross) in gross_by_player {
            let net = net_credits.get(&discord_id).copied().unwrap_or(gross);
            let penalty = gross.saturating_sub(net);
            if penalty > 0 {
                bankruptcy_penalties.insert(discord_id, penalty);
            }
        }

        Ok(SettlementResult {
            winners,
            losers,
            bankruptcy_penalties,
            vanity_taxes,
            low_priority_taxes,
            seed: SeedSettlement::default(),
        })
    }

    /// Persist only the betting component of a recorded match's JC snapshot.
    ///
    /// This is intentionally a narrow composition seam: Match remains the
    /// owner of the aggregate snapshot and later components, while Betting
    /// durably records its own settled-bet P/L before any fallible aggregate
    /// refresh.  A retry can therefore recover the base `bet` component even
    /// if the subsequent Match snapshot write fails.
    pub fn persist_settlement_bet_jc_changes(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        settlement: &SettlementResult,
    ) -> Result<(), BettingServiceRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let deltas = settlement_bet_jc_deltas(settlement);
        if deltas.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT jc_changes FROM matches
                 WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or_default();
        let mut changes = if raw.trim().is_empty() {
            BTreeMap::<String, BTreeMap<String, i64>>::new()
        } else {
            serde_json::from_str(&raw).map_err(|error| {
                BettingServiceRepositoryError::InvalidSnapshot(error.to_string())
            })?
        };
        for (discord_id, delta) in deltas {
            changes
                .entry(discord_id.to_string())
                .or_default()
                .insert("bet".to_owned(), delta);
        }
        let encoded = serde_json::to_string(&changes)
            .map_err(|error| BettingServiceRepositoryError::InvalidSnapshot(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE matches SET jc_changes = ?1
             WHERE match_id = ?2 AND guild_id = ?3",
            params![encoded, match_id, guild_id],
        )?;
        if changed != 1 {
            return Err(BettingServiceRepositoryError::MissingMatch { match_id, guild_id });
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn get_player_bet_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<BetHistoryEntry>, BettingServiceRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT b.bet_id, b.match_id, b.team_bet_on,
                    b.amount * COALESCE(b.leverage, 1), b.payout,
                    m.winning_team
             FROM bets b JOIN matches m ON m.match_id = b.match_id
             WHERE b.discord_id = ?1 AND b.guild_id = ?2 AND b.match_id IS NOT NULL
             ORDER BY b.bet_time, b.bet_id",
        )?;
        statement
            .query_map(
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    let team_text: String = row.get(2)?;
                    let team = parse_team(&team_text)?;
                    let effective_amount: i64 = row.get(3)?;
                    let payout: Option<i64> = row.get(4)?;
                    let winning_team: i64 = row.get(5)?;
                    let won = matches_winner(team, winning_team);
                    let profit = if won {
                        payout.unwrap_or_else(|| effective_amount.saturating_mul(2))
                            - effective_amount
                    } else {
                        -effective_amount
                    };
                    Ok(BetHistoryEntry {
                        bet_id: row.get(0)?,
                        match_id: row.get(1)?,
                        team,
                        effective_amount,
                        payout,
                        won,
                        profit,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn validate_wager(amount: i64, leverage: i64) -> Result<(), BettingServiceRepositoryError> {
    if amount <= 0 || leverage <= 0 {
        return Err(BettingServiceRepositoryError::InvalidWager);
    }
    amount
        .checked_mul(leverage)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)?;
    Ok(())
}

fn validate_bet_attribution(
    is_blind: bool,
    attribution: Option<&BetAttribution>,
) -> Result<(), BettingServiceRepositoryError> {
    let Some(attribution) = attribution else {
        return Ok(());
    };
    match (
        attribution.investment_target_id,
        attribution.investment_direction.as_deref(),
    ) {
        (None, None) => Ok(()),
        (None, Some(_)) | (Some(_), None) => Err(BettingServiceRepositoryError::InvalidWager),
        (Some(target_id), Some(direction))
            if is_blind && target_id > 0 && matches!(direction, "long" | "short") =>
        {
            Ok(())
        }
        _ => Err(BettingServiceRepositoryError::InvalidWager),
    }
}

fn balance_snapshot(
    transaction: &Transaction<'_>,
    guild_id: i64,
    discord_ids: &[i64],
) -> Result<BTreeMap<i64, i64>, rusqlite::Error> {
    let mut balances = BTreeMap::new();
    for discord_id in discord_ids {
        if balances.contains_key(discord_id) {
            continue;
        }
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(balance) = balance {
            balances.insert(*discord_id, balance);
        }
    }
    Ok(balances)
}

/// Python's integer `round(value * percentage / 100)` behavior, including
/// ties-to-even rather than the more common half-away-from-zero rule.
fn rounded_percentage(value: i64, percentage: i64) -> Result<i64, BettingServiceRepositoryError> {
    let numerator = value
        .checked_mul(percentage)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)?;
    let quotient = numerator / 100;
    let remainder = numerator % 100;
    let absolute_remainder = remainder.abs();
    let adjustment = if absolute_remainder > 50 || (absolute_remainder == 50 && quotient % 2 != 0) {
        numerator.signum()
    } else {
        0
    };
    quotient
        .checked_add(adjustment)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)
}

fn rounded_basis_points(
    value: i64,
    basis_points: i64,
) -> Result<i64, BettingServiceRepositoryError> {
    let numerator = value
        .checked_mul(basis_points)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)?;
    let quotient = numerator / 10_000;
    let remainder = numerator % 10_000;
    let absolute_remainder = remainder.abs();
    let adjustment =
        if absolute_remainder > 5_000 || (absolute_remainder == 5_000 && quotient % 2 != 0) {
            numerator.signum()
        } else {
            0
        };
    quotient
        .checked_add(adjustment)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)
}

#[derive(Clone, Copy, Debug)]
struct ExactAutomaticCandidate {
    discord_id: i64,
    team: BettingTeam,
    amount: Option<i64>,
    basis_points: Option<i64>,
}

struct ExactAutomaticContext<'transaction, 'connection, 'payload, 'balance> {
    transaction: &'transaction Transaction<'connection>,
    guild_id: i64,
    pending_match_id: Option<i64>,
    payload: Option<&'payload str>,
    bet_time: i64,
    candidate: ExactAutomaticCandidate,
    max_debt: i64,
    balances: &'balance BTreeMap<i64, i64>,
}

fn exact_automatic_candidate(
    context: ExactAutomaticContext<'_, '_, '_, '_>,
) -> Result<PendingBetRecord, BettingServiceRepositoryError> {
    let ExactAutomaticContext {
        transaction,
        guild_id,
        pending_match_id,
        payload,
        bet_time,
        candidate,
        max_debt,
        balances,
    } = context;
    if automatic_bet_already_exists(
        transaction,
        guild_id,
        pending_match_id,
        bet_time,
        candidate.discord_id,
    )? {
        return Err(BettingServiceRepositoryError::AutomaticBetAlreadyExists(
            candidate.discord_id,
        ));
    }
    let balance = balances.get(&candidate.discord_id).copied().ok_or(
        BettingServiceRepositoryError::MissingPlayer {
            discord_id: candidate.discord_id,
            guild_id,
        },
    )?;
    let amount = match (candidate.amount, candidate.basis_points) {
        (Some(amount), None) => amount,
        (None, Some(basis_points)) => rounded_basis_points(balance, basis_points)?,
        _ => return Err(BettingServiceRepositoryError::InvalidWager),
    };
    let request = PlaceBetRequest {
        guild_id: Some(guild_id),
        pending_match_id: pending_match_id.unwrap_or_default(),
        discord_id: candidate.discord_id,
        team: candidate.team,
        amount,
        bet_time,
        leverage: 1,
        max_debt,
        is_blind: true,
        odds_at_placement: automatic_odds_at_placement(
            transaction,
            guild_id,
            pending_match_id,
            candidate.team,
        )?,
    };
    if let Some(payload) = payload {
        validate_pending_bet(transaction, payload, request)?;
    }
    // Spectator automatic bets preserve Python's allow_negative=False rule.
    // `max_debt` remains part of the typed seam for the shared wager policy,
    // while the one-times stake must be fully covered by the live balance.
    insert_bet_in_transaction(transaction, guild_id, request, true, false)
}

fn automatic_odds_at_placement(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: Option<i64>,
    team: BettingTeam,
) -> Result<Option<f64>, BettingServiceRepositoryError> {
    let totals = pending_match_totals_in_transaction(transaction, guild_id, pending_match_id)?;
    let total_pool = totals.total();
    let team_total = match team {
        BettingTeam::Radiant => totals.radiant,
        BettingTeam::Dire => totals.dire,
    };
    Ok((total_pool > 0 && team_total > 0).then_some(total_pool as f64 / team_total as f64))
}

fn automatic_bet_already_exists(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: Option<i64>,
    bet_time: i64,
    discord_id: i64,
) -> Result<bool, rusqlite::Error> {
    if let Some(pending_match_id) = pending_match_id {
        transaction
            .query_row(
                "SELECT 1 FROM bets
                 WHERE guild_id = ?1 AND discord_id = ?2 AND match_id IS NULL
                   AND pending_match_id = ?3 AND is_blind = 1
                   AND investment_target_id IS NULL
                 LIMIT 1",
                params![guild_id, discord_id, pending_match_id],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
    } else {
        transaction
            .query_row(
                "SELECT 1 FROM bets
                 WHERE guild_id = ?1 AND discord_id = ?2 AND match_id IS NULL
                   AND pending_match_id IS NULL AND bet_time = ?3 AND is_blind = 1
                   AND investment_target_id IS NULL
                 LIMIT 1",
                params![guild_id, discord_id, bet_time],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
    }
}

fn automatic_candidate(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: Option<i64>,
    payload: Option<&str>,
    bet_time: i64,
    request: AutomaticBetRequest,
    balances: &BTreeMap<i64, i64>,
) -> Result<PendingBetRecord, BettingServiceRepositoryError> {
    let balance = balances.get(&request.discord_id).copied().ok_or(
        BettingServiceRepositoryError::MissingPlayer {
            discord_id: request.discord_id,
            guild_id,
        },
    )?;
    if request.percentage <= 0 || balance <= 0 {
        return Err(BettingServiceRepositoryError::InvalidWager);
    }
    let amount = balance
        .checked_mul(request.percentage)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)?
        .saturating_add(50)
        / 100;
    if amount <= 0 {
        return Err(BettingServiceRepositoryError::InvalidWager);
    }
    let request = PlaceBetRequest {
        guild_id: Some(guild_id),
        pending_match_id: pending_match_id.unwrap_or_default(),
        discord_id: request.discord_id,
        team: request.team,
        amount,
        bet_time,
        leverage: 1,
        max_debt: 500,
        is_blind: true,
        odds_at_placement: None,
    };
    if let Some(payload) = payload {
        validate_pending_bet(transaction, payload, request)?;
    }
    insert_bet_in_transaction(transaction, guild_id, request, true, false)
}

fn configured_investments_in_transaction(
    transaction: &Transaction<'_>,
    guild_id: i64,
    target_ids: &BTreeSet<i64>,
) -> Result<Vec<crate::autobet_investments::AutobetInvestment>, BettingServiceRepositoryError> {
    let placeholders = std::iter::repeat_n("?", target_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT guild_id, investor_id, target_id, direction, percentage
         FROM autobet_investments
         WHERE guild_id = ?1 AND target_id IN ({placeholders})
         ORDER BY investor_id ASC, target_id ASC"
    );
    let mut values = Vec::with_capacity(target_ids.len() + 1);
    values.push(rusqlite::types::Value::Integer(guild_id));
    values.extend(
        target_ids
            .iter()
            .copied()
            .map(rusqlite::types::Value::Integer),
    );
    let mut statement = transaction.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
        Ok(crate::autobet_investments::AutobetInvestment {
            guild_id: row.get(0)?,
            investor_id: row.get(1)?,
            target_id: row.get(2)?,
            direction: row.get(3)?,
            percentage: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn pending_match_totals_in_transaction(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: Option<i64>,
) -> Result<PotTotals, BettingServiceRepositoryError> {
    let Some(pending_match_id) = pending_match_id else {
        return Ok(PotTotals::default());
    };
    let mut statement = transaction.prepare(
        "SELECT team_bet_on, COALESCE(SUM(amount * COALESCE(leverage, 1)), 0)
         FROM bets
         WHERE guild_id = ?1 AND pending_match_id = ?2 AND match_id IS NULL
         GROUP BY team_bet_on",
    )?;
    let rows = statement.query_map(params![guild_id, pending_match_id], |row| {
        let team: String = row.get(0)?;
        let amount: i64 = row.get(1)?;
        Ok((team, amount))
    })?;
    let mut totals = PotTotals::default();
    for row in rows {
        let (team, amount) = row?;
        match team.to_ascii_lowercase().as_str() {
            "radiant" => totals.radiant = amount,
            "dire" => totals.dire = amount,
            _ => {}
        }
    }
    Ok(totals)
}

fn insert_bet_in_transaction(
    transaction: &Transaction<'_>,
    guild_id: i64,
    request: PlaceBetRequest,
    snapshot_sized: bool,
    allow_negative: bool,
) -> Result<PendingBetRecord, BettingServiceRepositoryError> {
    insert_bet_in_transaction_with_attribution(
        transaction,
        guild_id,
        request,
        snapshot_sized,
        allow_negative,
        None,
    )
}

fn insert_bet_in_transaction_with_attribution(
    transaction: &Transaction<'_>,
    guild_id: i64,
    request: PlaceBetRequest,
    snapshot_sized: bool,
    allow_negative: bool,
    attribution: Option<(i64, &str)>,
) -> Result<PendingBetRecord, BettingServiceRepositoryError> {
    let investment_target_id = attribution.as_ref().map(|(target_id, _)| *target_id);
    let investment_direction = attribution
        .as_ref()
        .map(|(_, direction)| (*direction).to_owned());
    validate_wager(request.amount, request.leverage)?;
    let stake = request
        .amount
        .checked_mul(request.leverage)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)?;
    let balance = transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0) FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![request.discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(BettingServiceRepositoryError::MissingPlayer {
            discord_id: request.discord_id,
            guild_id,
        })?;
    if balance < 0 && !allow_negative {
        return Err(BettingServiceRepositoryError::PlayerInDebt(
            request.discord_id,
        ));
    }
    let next_balance = balance
        .checked_sub(stake)
        .ok_or(BettingServiceRepositoryError::WagerOverflow)?;
    if request.leverage == 1 && next_balance < 0 && !allow_negative {
        return Err(BettingServiceRepositoryError::InsufficientBalance {
            discord_id: request.discord_id,
            balance,
            stake,
        });
    }
    if (request.leverage > 1 || allow_negative) && next_balance < -request.max_debt {
        return Err(BettingServiceRepositoryError::DebtLimit {
            discord_id: request.discord_id,
            max_debt: request.max_debt,
        });
    }
    set_ledger_context(
        transaction,
        "bet",
        Some(request.pending_match_id),
        if snapshot_sized {
            "automatic pending match bet"
        } else {
            "pending match bet"
        },
    )?;
    transaction.execute(
        "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3",
        params![next_balance, request.discord_id, guild_id],
    )?;
    clear_ledger_context(transaction)?;
    if let Some((target_id, direction)) = attribution {
        transaction.execute(
            "INSERT INTO bets (
                 guild_id, match_id, discord_id, team_bet_on, amount, bet_time,
                 leverage, is_blind, odds_at_placement, pending_match_id,
                 investment_target_id, investment_direction
             ) VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                guild_id,
                request.discord_id,
                team_name(request.team),
                request.amount,
                request.bet_time,
                request.leverage,
                i64::from(request.is_blind),
                request.odds_at_placement,
                (request.pending_match_id != 0).then_some(request.pending_match_id),
                target_id,
                direction,
            ],
        )?;
    } else {
        transaction.execute(
            "INSERT INTO bets (
                 guild_id, match_id, discord_id, team_bet_on, amount, bet_time,
                 leverage, is_blind, odds_at_placement, pending_match_id
             ) VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                guild_id,
                request.discord_id,
                team_name(request.team),
                request.amount,
                request.bet_time,
                request.leverage,
                i64::from(request.is_blind),
                request.odds_at_placement,
                (request.pending_match_id != 0).then_some(request.pending_match_id),
            ],
        )?;
    }
    Ok(PendingBetRecord {
        bet_id: transaction.last_insert_rowid(),
        guild_id,
        discord_id: request.discord_id,
        team: request.team,
        amount: request.amount,
        bet_time: request.bet_time,
        leverage: request.leverage,
        is_blind: request.is_blind,
        odds_at_placement: request.odds_at_placement,
        pending_match_id: (request.pending_match_id != 0).then_some(request.pending_match_id),
        investment_target_id,
        investment_direction,
    })
}

fn validate_pending_bet(
    transaction: &Transaction<'_>,
    payload: &str,
    request: PlaceBetRequest,
) -> Result<(), BettingServiceRepositoryError> {
    let lock_until = json_i64(payload, "bet_lock_until")
        .ok_or(BettingServiceRepositoryError::InvalidPendingPayload)?;
    if request.bet_time >= lock_until {
        return Err(BettingServiceRepositoryError::BettingClosed);
    }
    let radiant_ids = json_i64_array(payload, "radiant_team_ids");
    let dire_ids = json_i64_array(payload, "dire_team_ids");
    let participant_team = if radiant_ids.contains(&request.discord_id) {
        Some(BettingTeam::Radiant)
    } else if dire_ids.contains(&request.discord_id) {
        Some(BettingTeam::Dire)
    } else {
        None
    };
    if let Some(required) = participant_team
        && required != request.team
    {
        return Err(BettingServiceRepositoryError::ParticipantTeamOnly {
            discord_id: request.discord_id,
            required_team: team_name(required),
        });
    }
    let existing_team = transaction
        .query_row(
            "SELECT team_bet_on FROM bets
             WHERE guild_id = ?1 AND discord_id = ?2 AND match_id IS NULL
               AND pending_match_id = ?3 ORDER BY bet_time, bet_id LIMIT 1",
            params![
                BettingServiceRepository::normalize_guild_id(request.guild_id),
                request.discord_id,
                request.pending_match_id
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing_team
        .as_deref()
        .is_some_and(|team| team != team_name(request.team))
        && participant_team.is_some()
    {
        return Err(BettingServiceRepositoryError::OppositeTeamNotAllowed);
    }
    Ok(())
}

fn pending_bets_in_transaction(
    transaction: &Transaction<'_>,
    guild_id: i64,
    discord_id: Option<i64>,
    since_ts: i64,
    pending_match_id: Option<i64>,
) -> Result<Vec<PendingBetRecord>, rusqlite::Error> {
    let mut statement = match (discord_id, pending_match_id) {
        (Some(_), Some(_)) => transaction.prepare(
            "SELECT bet_id, guild_id, discord_id, team_bet_on, amount, bet_time,
                    COALESCE(leverage, 1), COALESCE(is_blind, 0),
                    odds_at_placement, pending_match_id,
                    investment_target_id, investment_direction
             FROM bets WHERE guild_id = ?1 AND discord_id = ?2 AND match_id IS NULL
               AND (pending_match_id = ?3 OR
                    (pending_match_id IS NULL AND bet_time >= ?4))
             ORDER BY bet_time, bet_id",
        )?,
        (Some(_), None) => transaction.prepare(
            "SELECT bet_id, guild_id, discord_id, team_bet_on, amount, bet_time,
                    COALESCE(leverage, 1), COALESCE(is_blind, 0),
                    odds_at_placement, pending_match_id,
                    investment_target_id, investment_direction
             FROM bets WHERE guild_id = ?1 AND discord_id = ?2 AND match_id IS NULL
               AND bet_time >= ?3 ORDER BY bet_time, bet_id",
        )?,
        (None, Some(_)) => transaction.prepare(
            "SELECT bet_id, guild_id, discord_id, team_bet_on, amount, bet_time,
                    COALESCE(leverage, 1), COALESCE(is_blind, 0),
                    odds_at_placement, pending_match_id,
                    investment_target_id, investment_direction
             FROM bets WHERE guild_id = ?1 AND match_id IS NULL
               AND (pending_match_id = ?2 OR
                    (pending_match_id IS NULL AND bet_time >= ?3))
             ORDER BY bet_time, bet_id",
        )?,
        (None, None) => transaction.prepare(
            "SELECT bet_id, guild_id, discord_id, team_bet_on, amount, bet_time,
                    COALESCE(leverage, 1), COALESCE(is_blind, 0),
                    odds_at_placement, pending_match_id,
                    investment_target_id, investment_direction
             FROM bets WHERE guild_id = ?1 AND match_id IS NULL AND bet_time >= ?2
             ORDER BY bet_time, bet_id",
        )?,
    };
    match (discord_id, pending_match_id) {
        (Some(discord_id), Some(pending_match_id)) => statement
            .query_map(
                params![guild_id, discord_id, pending_match_id, since_ts],
                pending_bet_from_row,
            )?
            .collect(),
        (Some(discord_id), None) => statement
            .query_map(
                params![guild_id, discord_id, since_ts],
                pending_bet_from_row,
            )?
            .collect(),
        (None, Some(pending_match_id)) => statement
            .query_map(
                params![guild_id, pending_match_id, since_ts],
                pending_bet_from_row,
            )?
            .collect(),
        (None, None) => statement
            .query_map(params![guild_id, since_ts], pending_bet_from_row)?
            .collect(),
    }
}

fn pending_bet_from_row(row: &rusqlite::Row<'_>) -> Result<PendingBetRecord, rusqlite::Error> {
    let team_text: String = row.get(3)?;
    Ok(PendingBetRecord {
        bet_id: row.get(0)?,
        guild_id: row.get(1)?,
        discord_id: row.get(2)?,
        team: parse_team(&team_text)?,
        amount: row.get(4)?,
        bet_time: row.get(5)?,
        leverage: row.get(6)?,
        is_blind: row.get::<_, i64>(7)? != 0,
        odds_at_placement: row.get(8)?,
        pending_match_id: row.get(9)?,
        investment_target_id: row.get(10)?,
        investment_direction: row.get(11)?,
    })
}

fn delete_pending_rows(
    transaction: &Transaction<'_>,
    guild_id: i64,
    since_ts: i64,
    pending_match_id: Option<i64>,
) -> Result<usize, rusqlite::Error> {
    match pending_match_id {
        Some(pending_match_id) => transaction.execute(
            "DELETE FROM bets WHERE guild_id = ?1 AND match_id IS NULL
             AND (pending_match_id = ?2 OR
                  (pending_match_id IS NULL AND bet_time >= ?3))",
            params![guild_id, pending_match_id, since_ts],
        ),
        None => transaction.execute(
            "DELETE FROM bets WHERE guild_id = ?1 AND match_id IS NULL AND bet_time >= ?2",
            params![guild_id, since_ts],
        ),
    }
}

struct PayoutCalculation<'a> {
    bets: &'a [PendingBetRecord],
    winning_team: BettingTeam,
    mode: BettingMode,
    total_pool: i64,
    winning_pool: i64,
    winner_seed: i64,
    house_payout_multiplier: f64,
    payout_multiplier: f64,
}

fn calculate_payouts(request: PayoutCalculation<'_>) -> BTreeMap<i64, i64> {
    let mut payouts = BTreeMap::new();
    if request.winning_pool <= 0 {
        return payouts;
    }
    match request.mode {
        BettingMode::House => {
            let user_stakes = ordered_winner_stakes(request.bets, request.winning_team);
            let mut allocated_seed = 0_i64;
            let mut seed_by_user = BTreeMap::new();
            for (index, (discord_id, user_stake)) in user_stakes.iter().enumerate() {
                let seed = if index + 1 == user_stakes.len() {
                    request.winner_seed.saturating_sub(allocated_seed)
                } else {
                    proportional_floor(*user_stake, request.winner_seed, request.winning_pool)
                };
                allocated_seed = allocated_seed.saturating_add(seed);
                seed_by_user.insert(*discord_id, seed);
            }
            for (discord_id, user_stake) in user_stakes {
                let user_bets = request
                    .bets
                    .iter()
                    .filter(|bet| bet.team == request.winning_team && bet.discord_id == discord_id)
                    .collect::<Vec<_>>();
                let user_seed = seed_by_user.get(&discord_id).copied().unwrap_or_default();
                let mut allocated_user_seed = 0_i64;
                for (index, bet) in user_bets.iter().enumerate() {
                    let bet_seed = if index + 1 == user_bets.len() {
                        user_seed.saturating_sub(allocated_user_seed)
                    } else {
                        proportional_floor(bet.effective_amount(), user_seed, user_stake)
                    };
                    allocated_user_seed = allocated_user_seed.saturating_add(bet_seed);
                    let house_payout = (bet.effective_amount() as f64
                        * (1.0 + request.house_payout_multiplier))
                        as i64;
                    payouts.insert(
                        bet.bet_id,
                        scaled_payout(
                            house_payout.saturating_add(bet_seed),
                            request.payout_multiplier,
                        ),
                    );
                }
            }
        }
        BettingMode::Pool => {
            // Python parity (`bet_repository.py::_calculate_pool_payouts`):
            // per-bet raw payouts are IEEE doubles summed per user in bet
            // order, ceiled with `math.ceil`, and the per-bet split floors
            // `raw / user_raw_total * user_final`. The float path is
            // deliberate — an exact integer ceiling rounds differently on
            // ~0.7% of settlements (Python's double rounding can land 1 ULP
            // above an integer and `ceil` promotes it), and the lift-and-shift
            // contract preserves Python's observable credits.
            let seeded_pool = request.total_pool.saturating_add(request.winner_seed) as f64;
            let winning_pool = request.winning_pool as f64;
            for (discord_id, _user_stake) in
                ordered_winner_stakes(request.bets, request.winning_team)
            {
                let user_bets = request
                    .bets
                    .iter()
                    .filter(|bet| bet.team == request.winning_team && bet.discord_id == discord_id)
                    .collect::<Vec<_>>();
                let raw_payouts = user_bets
                    .iter()
                    .map(|bet| (bet.effective_amount() as f64 / winning_pool) * seeded_pool)
                    .collect::<Vec<_>>();
                let mut user_raw_total = 0.0_f64;
                for raw_payout in &raw_payouts {
                    user_raw_total += raw_payout;
                }
                let normal_payout = user_raw_total.ceil() as i64;
                let user_payout = scaled_payout(normal_payout, request.payout_multiplier);
                let mut allocated = 0_i64;
                for (index, bet) in user_bets.iter().enumerate() {
                    let payout = if index + 1 == user_bets.len() {
                        user_payout.saturating_sub(allocated)
                    } else if user_raw_total != 0.0 {
                        ((raw_payouts[index] / user_raw_total) * user_payout as f64) as i64
                    } else {
                        0
                    };
                    allocated = allocated.saturating_add(payout);
                    payouts.insert(bet.bet_id, payout);
                }
            }
        }
    }
    payouts
}

fn ordered_winner_stakes(bets: &[PendingBetRecord], winning_team: BettingTeam) -> Vec<(i64, i64)> {
    let mut stakes = Vec::<(i64, i64)>::new();
    for bet in bets.iter().filter(|bet| bet.team == winning_team) {
        if let Some((_, stake)) = stakes
            .iter_mut()
            .find(|(discord_id, _)| *discord_id == bet.discord_id)
        {
            *stake = stake.saturating_add(bet.effective_amount());
        } else {
            stakes.push((bet.discord_id, bet.effective_amount()));
        }
    }
    stakes
}

fn proportional_floor(part: i64, total: i64, denominator: i64) -> i64 {
    if part <= 0 || total <= 0 || denominator <= 0 {
        return 0;
    }
    (part as f64 / denominator as f64 * total as f64) as i64
}

fn normalized_house_multiplier(multiplier: f64) -> f64 {
    if multiplier.is_finite() {
        multiplier
    } else {
        1.0
    }
}

fn normalized_rate(rate: f64, maximum: f64) -> f64 {
    if rate.is_finite() {
        rate.clamp(0.0, maximum)
    } else {
        maximum
    }
}

fn normalized_payout_multiplier(multiplier: f64) -> f64 {
    if multiplier.is_finite() {
        multiplier.clamp(0.0, 10.0)
    } else {
        1.0
    }
}

fn scaled_payout(payout: i64, multiplier: f64) -> i64 {
    ((payout as f64 * multiplier) as i64).max(0)
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    related_id: Option<i64>,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT OR REPLACE INTO economy_ledger_context
             (id, source, related_type, related_id, reason)
         VALUES (1, ?1, 'pending_match', ?2, ?3)",
        params![source, related_id.map(|value| value.to_string()), reason],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context WHERE id = 1", [])?;
    Ok(())
}

fn team_name(team: BettingTeam) -> &'static str {
    match team {
        BettingTeam::Radiant => "radiant",
        BettingTeam::Dire => "dire",
    }
}

fn parse_team(value: &str) -> Result<BettingTeam, rusqlite::Error> {
    match value {
        "radiant" => Ok(BettingTeam::Radiant),
        "dire" => Ok(BettingTeam::Dire),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn settlement_bet_jc_deltas(settlement: &SettlementResult) -> BTreeMap<i64, i64> {
    let mut deltas = BTreeMap::new();
    for bet in settlement.winners.iter().chain(&settlement.losers) {
        deltas
            .entry(bet.discord_id)
            .and_modify(|delta: &mut i64| *delta = delta.saturating_sub(bet.effective_amount))
            .or_insert(-bet.effective_amount);
    }
    for bet in &settlement.winners {
        deltas
            .entry(bet.discord_id)
            .and_modify(|delta| *delta = delta.saturating_add(bet.payout))
            .or_insert(bet.payout);
    }
    for deductions in [
        &settlement.bankruptcy_penalties,
        &settlement.vanity_taxes,
        &settlement.low_priority_taxes,
    ] {
        for (discord_id, amount) in deductions {
            deltas
                .entry(*discord_id)
                .and_modify(|delta| *delta = delta.saturating_sub(*amount))
                .or_insert(-*amount);
        }
    }
    deltas
}

const fn matches_winner(team: BettingTeam, winning_team: i64) -> bool {
    matches!(
        (team, winning_team),
        (BettingTeam::Radiant, 1) | (BettingTeam::Dire, 2)
    )
}

fn is_candidate_skip(error: &BettingServiceRepositoryError) -> bool {
    matches!(
        error,
        BettingServiceRepositoryError::InvalidWager
            | BettingServiceRepositoryError::ParticipantTeamOnly { .. }
            | BettingServiceRepositoryError::OppositeTeamNotAllowed
            | BettingServiceRepositoryError::MissingPlayer { .. }
            | BettingServiceRepositoryError::PlayerInDebt(_)
            | BettingServiceRepositoryError::InsufficientBalance { .. }
            | BettingServiceRepositoryError::DebtLimit { .. }
            | BettingServiceRepositoryError::AutomaticBetAlreadyExists(_)
    )
}

fn json_value<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\"");
    let start = raw.find(&marker)? + marker.len();
    let remainder = raw[start..].trim_start().strip_prefix(':')?.trim_start();
    if remainder.starts_with('[') {
        let end = remainder.find(']')?;
        return Some(&remainder[..=end]);
    }
    let end = remainder.find([',', '}']).unwrap_or(remainder.len());
    Some(remainder[..end].trim())
}

fn json_i64(raw: &str, key: &str) -> Option<i64> {
    json_value(raw, key)?.parse().ok()
}

fn json_i64_array(raw: &str, key: &str) -> Vec<i64> {
    json_value(raw, key)
        .and_then(|value| value.strip_prefix('[')?.strip_suffix(']'))
        .map(|body| {
            body.split(',')
                .filter_map(|value| value.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "betting_service/tests.rs"]
mod tests;
