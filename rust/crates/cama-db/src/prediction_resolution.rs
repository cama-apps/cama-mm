//! Existing-schema prediction expiry, settlement, and rollback persistence.
//!
//! Python remains the migration and live runtime authority. This adapter opens
//! only an already-migrated database and places every multi-row mutation behind
//! `BEGIN IMMEDIATE`. Settlement credits, audit context, position deductions,
//! market state, and rollback ladder restoration therefore succeed or fail as
//! one SQLite unit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;
use crate::predictions_repository::{CONTRACT_VALUE, ContractSide, NewLevel};
use cama_db_core::profit_deductions::penalty_games_remaining;

pub const BASIS_POINTS: i64 = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettlementAdjustments {
    /// JC paid for one winning contract before the event multiplier.
    pub contract_value: i64,
    /// `10_000` is 1x; `100_000` is 10x.
    pub payout_multiplier_bps: i64,
    /// Basis points of positive profit withheld per outstanding penalty game.
    /// `None` disables the bankruptcy lookup and withholding.
    pub bankruptcy_penalty_bps_per_game: Option<i64>,
    pub vanity_tax_bps: i64,
    pub vanity_taxable_ids: BTreeSet<i64>,
    pub low_priority_tax_bps: i64,
    pub low_priority_taxable_ids: BTreeSet<i64>,
}

impl Default for SettlementAdjustments {
    fn default() -> Self {
        Self {
            contract_value: CONTRACT_VALUE,
            payout_multiplier_bps: BASIS_POINTS,
            bankruptcy_penalty_bps_per_game: None,
            vanity_tax_bps: 0,
            vanity_taxable_ids: BTreeSet::new(),
            low_priority_tax_bps: 0,
            low_priority_taxable_ids: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionWinner {
    pub discord_id: i64,
    pub contracts: i64,
    /// Net player credit after bankruptcy, vanity, and low-priority deductions.
    pub payout: i64,
    pub profit: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolutionLoser {
    pub discord_id: i64,
    pub contracts: i64,
    pub loss: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolutionParticipant {
    pub discord_id: i64,
    pub yes_contracts: i64,
    pub no_contracts: i64,
    pub winning_contracts: i64,
    pub cost_basis: i64,
    pub payout: i64,
    pub profit: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    pub prediction_id: i64,
    pub guild_id: i64,
    pub outcome: ContractSide,
    pub winners: Vec<ResolutionWinner>,
    pub losers: Vec<ResolutionLoser>,
    pub participants: Vec<ResolutionParticipant>,
    /// Gross payout before bankruptcy, vanity, and low-priority deductions,
    /// matching Python.
    pub total_payout: i64,
    pub payout_multiplier_bps: i64,
    pub lp_pnl: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollbackResult {
    pub prediction_id: i64,
    pub guild_id: i64,
    pub previous_outcome: ContractSide,
    pub total_reversed: i64,
    pub affected_players: i64,
    pub lp_pnl: i64,
    pub current_price: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UserOrderbookStats {
    pub realized_pnl: i64,
    pub wins: i64,
    pub losses: i64,
    pub resolved_markets: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PnlHistoryPoint {
    pub prediction_id: i64,
    pub settle_time: i64,
    pub delta: i64,
}

#[derive(Debug, Error)]
pub enum PredictionResolutionError {
    #[error("Prediction not found.")]
    PredictionNotFound,
    #[error("Cannot settle market in status '{0}'.")]
    CannotSettle(String),
    #[error("Cannot rollback market in status '{0}'.")]
    CannotRollback(String),
    #[error("Resolved prediction has no valid outcome.")]
    MissingOutcome,
    #[error("Winning player not found.")]
    WinningPlayerNotFound,
    #[error("Winning player {0} no longer exists.")]
    WinningPlayerDeleted(i64),
    #[error("Winning player {0} account was re-created after settlement.")]
    WinningPlayerRecreated(i64),
    #[error("Settlement ledger is inconsistent.")]
    SettlementLedgerInconsistent,
    #[error("Vanity tax ledger is inconsistent.")]
    VanityTaxLedgerInconsistent,
    #[error("Low priority tax ledger is inconsistent.")]
    LowPriorityTaxLedgerInconsistent,
    #[error("settlement basis-point values must be between zero and a signed 64-bit integer")]
    InvalidAdjustments,
    #[error("invalid rollback book level")]
    InvalidLevel,
    #[error("prediction resolution arithmetic overflowed")]
    ArithmeticOverflow,
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("prediction resolution SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct PredictionResolutionRepository {
    path: PathBuf,
}

impl PredictionResolutionRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Atomically lock expired open markets for one guild, newest first.
    pub fn lock_expired_predictions(
        &self,
        guild_id: i64,
        now: i64,
    ) -> Result<Vec<i64>, PredictionResolutionError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prediction_ids = {
            let mut statement = transaction.prepare(
                "SELECT prediction_id FROM predictions
                 WHERE guild_id=?1 AND status='open' AND closes_at<=?2
                 ORDER BY created_at DESC",
            )?;
            statement
                .query_map(params![guild_id, now], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if !prediction_ids.is_empty() {
            transaction.execute(
                "UPDATE predictions SET status='locked'
                 WHERE guild_id=?1 AND status='open' AND closes_at<=?2",
                params![guild_id, now],
            )?;
        }
        transaction.commit()?;
        Ok(prediction_ids)
    }

    pub fn settle_orderbook(
        &self,
        prediction_id: i64,
        outcome: ContractSide,
        resolved_by: Option<i64>,
        adjustments: &SettlementAdjustments,
    ) -> Result<Resolution, PredictionResolutionError> {
        self.settle_orderbook_at(
            prediction_id,
            outcome,
            resolved_by,
            adjustments,
            unix_timestamp()?,
        )
    }

    pub fn settle_orderbook_at(
        &self,
        prediction_id: i64,
        outcome: ContractSide,
        resolved_by: Option<i64>,
        adjustments: &SettlementAdjustments,
        now: i64,
    ) -> Result<Resolution, PredictionResolutionError> {
        validate_adjustments(adjustments)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, guild_id, lp_pnl): (String, i64, i64) = transaction
            .query_row(
                "SELECT status,guild_id,COALESCE(lp_pnl,0)
                 FROM predictions WHERE prediction_id=?1",
                [prediction_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(PredictionResolutionError::PredictionNotFound)?;
        if status != "open" && status != "locked" {
            return Err(PredictionResolutionError::CannotSettle(status));
        }

        transaction.execute(
            "DELETE FROM prediction_levels WHERE prediction_id=?1",
            [prediction_id],
        )?;
        let positions = load_positions(&transaction, prediction_id)?;
        let mut winners = Vec::new();
        let mut losers = Vec::new();
        let mut participants = Vec::with_capacity(positions.len());
        let mut total_payout = 0_i64;

        for position in positions {
            let winning_contracts = match outcome {
                ContractSide::Yes => position.yes_contracts,
                ContractSide::No => position.no_contracts,
            };
            let losing_contracts = match outcome {
                ContractSide::Yes => position.no_contracts,
                ContractSide::No => position.yes_contracts,
            };
            let winning_basis = match outcome {
                ContractSide::Yes => position.yes_cost_basis_total,
                ContractSide::No => position.no_cost_basis_total,
            };
            let losing_basis = match outcome {
                ContractSide::Yes => position.no_cost_basis_total,
                ContractSide::No => position.yes_cost_basis_total,
            };
            let base_payout = winning_contracts
                .checked_mul(adjustments.contract_value)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            let gross_payout =
                multiply_bps_round_ties_even(base_payout, adjustments.payout_multiplier_bps)?;
            let cost_basis = winning_basis
                .checked_add(losing_basis)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            let gross_profit = gross_payout
                .checked_sub(cost_basis)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            let mut bankruptcy_penalty = 0_i64;
            let mut vanity_tax = 0_i64;
            let mut low_priority_tax = 0_i64;

            if base_payout > 0 {
                if gross_profit > 0
                    && let Some(bps_per_game) = adjustments.bankruptcy_penalty_bps_per_game
                {
                    let remaining =
                        penalty_games_remaining(&transaction, guild_id, position.discord_id)?;
                    bankruptcy_penalty =
                        bankruptcy_withholding(gross_profit, bps_per_game, remaining)?;
                }
                if gross_profit > 0
                    && adjustments
                        .vanity_taxable_ids
                        .contains(&position.discord_id)
                {
                    vanity_tax = multiply_bps_floor(gross_profit, adjustments.vanity_tax_bps)?;
                }
                if gross_profit > 0
                    && adjustments
                        .low_priority_taxable_ids
                        .contains(&position.discord_id)
                {
                    // Billed on the same gross profit the vanity tax uses, then
                    // capped at the profit the earlier withholdings left so the
                    // three deductions together can never exceed that profit.
                    let remaining_profit = gross_profit
                        .checked_sub(bankruptcy_penalty)
                        .and_then(|remaining| remaining.checked_sub(vanity_tax))
                        .ok_or(PredictionResolutionError::ArithmeticOverflow)?
                        .max(0);
                    low_priority_tax =
                        multiply_bps_floor(gross_profit, adjustments.low_priority_tax_bps)?
                            .min(remaining_profit);
                }

                let credited_before_tax = gross_payout
                    .checked_sub(bankruptcy_penalty)
                    .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
                let metadata = settlement_metadata(
                    outcome,
                    base_payout,
                    gross_payout,
                    adjustments.payout_multiplier_bps,
                    bankruptcy_penalty,
                    vanity_tax,
                    low_priority_tax,
                    winning_contracts,
                );
                if credited_before_tax > 0 {
                    update_player_with_context(
                        &transaction,
                        LedgeredBalanceMutation {
                            guild_id,
                            discord_id: position.discord_id,
                            delta: credited_before_tax,
                            source: "prediction_resolution",
                            actor_id: None,
                            prediction_id,
                            reason: "prediction resolution payout",
                            metadata: &metadata,
                        },
                    )?;
                } else {
                    insert_zero_settlement_ledger(
                        &transaction,
                        guild_id,
                        position.discord_id,
                        prediction_id,
                        &metadata,
                        now,
                    )?;
                }
                if vanity_tax > 0 {
                    update_player_with_context(
                        &transaction,
                        LedgeredBalanceMutation {
                            guild_id,
                            discord_id: position.discord_id,
                            delta: -vanity_tax,
                            source: "vanity_tax",
                            actor_id: None,
                            prediction_id,
                            reason: "vanity tax on JC profit",
                            metadata: &metadata,
                        },
                    )?;
                }
                if low_priority_tax > 0 {
                    update_player_with_context(
                        &transaction,
                        LedgeredBalanceMutation {
                            guild_id,
                            discord_id: position.discord_id,
                            delta: -low_priority_tax,
                            source: "low_priority_tax",
                            actor_id: None,
                            prediction_id,
                            reason: "low priority tax on JC profit",
                            metadata: &metadata,
                        },
                    )?;
                }
                transaction.execute(
                    "UPDATE prediction_positions
                     SET bankruptcy_penalty=?1,vanity_tax=?2,low_priority_tax=?3
                     WHERE prediction_id=?4 AND discord_id=?5",
                    params![
                        bankruptcy_penalty,
                        vanity_tax,
                        low_priority_tax,
                        prediction_id,
                        position.discord_id
                    ],
                )?;

                let net_payout = credited_before_tax
                    .checked_sub(vanity_tax)
                    .and_then(|credited| credited.checked_sub(low_priority_tax))
                    .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
                winners.push(ResolutionWinner {
                    discord_id: position.discord_id,
                    contracts: winning_contracts,
                    payout: net_payout,
                    profit: gross_profit - bankruptcy_penalty - vanity_tax - low_priority_tax,
                    bankruptcy_penalty,
                    vanity_tax,
                    low_priority_tax,
                });
            }
            if losing_contracts > 0 {
                losers.push(ResolutionLoser {
                    discord_id: position.discord_id,
                    contracts: losing_contracts,
                    loss: losing_basis,
                });
            }

            let net_payout = if gross_payout > 0 {
                gross_payout - bankruptcy_penalty - vanity_tax - low_priority_tax
            } else {
                0
            };
            participants.push(ResolutionParticipant {
                discord_id: position.discord_id,
                yes_contracts: position.yes_contracts,
                no_contracts: position.no_contracts,
                winning_contracts,
                cost_basis,
                payout: net_payout,
                profit: net_payout - cost_basis,
                bankruptcy_penalty,
                vanity_tax,
                low_priority_tax,
            });
            total_payout = total_payout
                .checked_add(gross_payout)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
        }

        let new_lp_pnl = lp_pnl
            .checked_sub(total_payout)
            .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
        transaction.execute(
            "UPDATE predictions
             SET status='resolved',outcome=?1,resolved_at=?2,resolved_by=?3,lp_pnl=?4
             WHERE prediction_id=?5",
            params![
                side_name(outcome),
                now,
                resolved_by,
                new_lp_pnl,
                prediction_id
            ],
        )?;
        transaction.commit()?;

        Ok(Resolution {
            prediction_id,
            guild_id,
            outcome,
            winners,
            losers,
            participants,
            total_payout,
            payout_multiplier_bps: adjustments.payout_multiplier_bps,
            lp_pnl: new_lp_pnl,
        })
    }

    pub fn rollback_orderbook(
        &self,
        prediction_id: i64,
        guild_id: i64,
        levels: &[NewLevel],
        rolled_back_by: Option<i64>,
    ) -> Result<RollbackResult, PredictionResolutionError> {
        self.rollback_orderbook_at(
            prediction_id,
            guild_id,
            levels,
            rolled_back_by,
            unix_timestamp()?,
        )
    }

    pub fn rollback_orderbook_at(
        &self,
        prediction_id: i64,
        guild_id: i64,
        levels: &[NewLevel],
        rolled_back_by: Option<i64>,
        now: i64,
    ) -> Result<RollbackResult, PredictionResolutionError> {
        validate_levels(levels)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let market: Option<(String, Option<String>, Option<i64>, i64)> = transaction
            .query_row(
                "SELECT status,outcome,current_price,COALESCE(lp_pnl,0)
                 FROM predictions WHERE prediction_id=?1 AND guild_id=?2",
                params![prediction_id, guild_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((status, outcome, current_price, lp_pnl)) = market else {
            return Err(PredictionResolutionError::PredictionNotFound);
        };
        if status != "resolved" {
            return Err(PredictionResolutionError::CannotRollback(status));
        }
        let outcome = parse_side(outcome.as_deref())?;
        let current_price = current_price.ok_or(PredictionResolutionError::MissingOutcome)?;
        let positions = load_rollback_positions(&transaction, prediction_id)?;
        let positions_by_account = positions
            .iter()
            .map(|position| (position.discord_id, *position))
            .collect::<BTreeMap<_, _>>();
        // prediction_trades.jopacoins is already signed -- buys store the cost
        // and sells store the negated proceeds -- so the signed cash flow is a
        // plain sum. Negating sells here would count them twice, inflating the
        // expected gross and failing the settlement audit on any market that
        // contains a sell, which permanently blocks its rollback.
        let trade_cash: i64 = transaction.query_row(
            "SELECT COALESCE(SUM(jopacoins),0)
             FROM prediction_trades WHERE prediction_id=?1",
            [prediction_id],
            |row| row.get(0),
        )?;
        let expected_total_gross = trade_cash
            .checked_sub(lp_pnl)
            .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
        let ledger_floor = latest_rollback_ledger_id(&transaction, guild_id, prediction_id)?;
        let vanity_rows = load_ledger_rows(
            &transaction,
            guild_id,
            prediction_id,
            "vanity_tax",
            ledger_floor,
        )?;
        let mut vanity_by_account = BTreeMap::new();
        for row in vanity_rows {
            let account_id = row
                .player_account_id()
                .ok_or(PredictionResolutionError::VanityTaxLedgerInconsistent)?;
            if vanity_by_account.contains_key(&account_id) {
                return Err(PredictionResolutionError::VanityTaxLedgerInconsistent);
            }
            let position = positions_by_account
                .get(&account_id)
                .ok_or(PredictionResolutionError::VanityTaxLedgerInconsistent)?;
            if position.winning_contracts(outcome) <= 0
                || position.vanity_tax <= 0
                || row.delta != -position.vanity_tax
            {
                return Err(PredictionResolutionError::VanityTaxLedgerInconsistent);
            }
            vanity_by_account.insert(account_id, row);
        }

        let low_priority_rows = load_ledger_rows(
            &transaction,
            guild_id,
            prediction_id,
            "low_priority_tax",
            ledger_floor,
        )?;
        let mut low_priority_by_account = BTreeMap::new();
        for row in low_priority_rows {
            let account_id = row
                .player_account_id()
                .ok_or(PredictionResolutionError::LowPriorityTaxLedgerInconsistent)?;
            if low_priority_by_account.contains_key(&account_id) {
                return Err(PredictionResolutionError::LowPriorityTaxLedgerInconsistent);
            }
            let position = positions_by_account
                .get(&account_id)
                .ok_or(PredictionResolutionError::LowPriorityTaxLedgerInconsistent)?;
            if position.winning_contracts(outcome) <= 0
                || position.low_priority_tax <= 0
                || row.delta != -position.low_priority_tax
            {
                return Err(PredictionResolutionError::LowPriorityTaxLedgerInconsistent);
            }
            low_priority_by_account.insert(account_id, row);
        }

        let settlement_rows = load_ledger_rows(
            &transaction,
            guild_id,
            prediction_id,
            "prediction_resolution",
            ledger_floor,
        )?;
        let mut settlements_by_account = BTreeMap::new();
        let mut reversals = Vec::new();
        let mut total_gross_payout = 0_i64;
        for row in settlement_rows {
            let account_id = row
                .player_account_id()
                .ok_or(PredictionResolutionError::SettlementLedgerInconsistent)?;
            if settlements_by_account.contains_key(&account_id) {
                return Err(PredictionResolutionError::SettlementLedgerInconsistent);
            }
            let position = positions_by_account
                .get(&account_id)
                .ok_or(PredictionResolutionError::SettlementLedgerInconsistent)?;
            let winning_contracts = position.winning_contracts(outcome);
            if winning_contracts <= 0 {
                return Err(PredictionResolutionError::SettlementLedgerInconsistent);
            }
            let fallback_gross = winning_contracts
                .checked_mul(CONTRACT_VALUE)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            let gross_payout = row.gross_payout.unwrap_or(fallback_gross);
            let bankruptcy_penalty = row
                .bankruptcy_penalty
                .unwrap_or(position.bankruptcy_penalty);
            let has_tax_row = vanity_by_account.contains_key(&account_id);
            let has_low_priority_tax_row = low_priority_by_account.contains_key(&account_id);
            if has_tax_row != (position.vanity_tax > 0)
                || has_low_priority_tax_row != (position.low_priority_tax > 0)
                || gross_payout < 0
                || bankruptcy_penalty < 0
                || row.delta != gross_payout - bankruptcy_penalty
            {
                return Err(PredictionResolutionError::SettlementLedgerInconsistent);
            }
            total_gross_payout = total_gross_payout
                .checked_add(gross_payout)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            reversals.push(Reversal {
                discord_id: account_id,
                settlement_ledger_id: row.ledger_id,
                settlement_credit: row.delta,
                gross_payout,
                bankruptcy_penalty,
                vanity_tax: position.vanity_tax,
                low_priority_tax: position.low_priority_tax,
                base_gross_payout: row.base_gross_payout.unwrap_or(fallback_gross),
                payout_multiplier_bps: row.payout_multiplier_bps.unwrap_or(BASIS_POINTS),
            });
            settlements_by_account.insert(account_id, row);
        }
        if total_gross_payout != expected_total_gross
            || positions
                .iter()
                .filter(|position| position.winning_contracts(outcome) > 0)
                .any(|position| !settlements_by_account.contains_key(&position.discord_id))
        {
            return Err(PredictionResolutionError::SettlementLedgerInconsistent);
        }

        for reversal in &reversals {
            let player_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
                params![reversal.discord_id, guild_id],
                |row| row.get(0),
            )?;
            if !player_exists {
                return Err(PredictionResolutionError::WinningPlayerDeleted(
                    reversal.discord_id,
                ));
            }
            let recreated: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM economy_ledger_entries
                     WHERE guild_id=?1 AND account_type='player' AND account_id=?2
                       AND source='player_insert' AND ledger_id>?3)",
                params![guild_id, reversal.discord_id, reversal.settlement_ledger_id],
                |row| row.get(0),
            )?;
            if recreated {
                return Err(PredictionResolutionError::WinningPlayerRecreated(
                    reversal.discord_id,
                ));
            }
        }

        let mut total_reversed = 0_i64;
        let mut affected_players = 0_i64;
        for reversal in reversals {
            let credited = reversal
                .settlement_credit
                .checked_sub(reversal.vanity_tax)
                .and_then(|credited| credited.checked_sub(reversal.low_priority_tax))
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            let metadata = settlement_metadata(
                outcome,
                reversal.base_gross_payout,
                reversal.gross_payout,
                reversal.payout_multiplier_bps,
                reversal.bankruptcy_penalty,
                reversal.vanity_tax,
                reversal.low_priority_tax,
                0,
            );
            if credited == 0 {
                insert_zero_rollback_ledger(
                    &transaction,
                    guild_id,
                    reversal.discord_id,
                    prediction_id,
                    rolled_back_by,
                    &metadata,
                    now,
                )?;
                continue;
            }
            update_player_with_context(
                &transaction,
                LedgeredBalanceMutation {
                    guild_id,
                    discord_id: reversal.discord_id,
                    delta: -credited,
                    source: "prediction_resolution_rollback",
                    actor_id: rolled_back_by,
                    prediction_id,
                    reason: "prediction resolution rollback",
                    metadata: &metadata,
                },
            )?;
            total_reversed = total_reversed
                .checked_add(credited)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            affected_players = affected_players
                .checked_add(1)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
        }

        transaction.execute(
            "UPDATE prediction_positions
             SET bankruptcy_penalty=0,vanity_tax=0,low_priority_tax=0
             WHERE prediction_id=?1",
            [prediction_id],
        )?;
        transaction.execute(
            "DELETE FROM prediction_levels WHERE prediction_id=?1",
            [prediction_id],
        )?;
        for level in levels {
            transaction.execute(
                "INSERT INTO prediction_levels
                     (prediction_id,side,price,remaining_size,posted_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    prediction_id,
                    level.side.as_str(),
                    level.price,
                    level.size,
                    now
                ],
            )?;
        }
        let restored_lp_pnl = lp_pnl
            .checked_add(total_gross_payout)
            .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
        transaction.execute(
            "UPDATE predictions
             SET status='open',outcome=NULL,resolved_at=NULL,resolved_by=NULL,
                 lp_pnl=?1,last_refresh_at=?2
             WHERE prediction_id=?3",
            params![restored_lp_pnl, now, prediction_id],
        )?;
        transaction.execute(
            "INSERT INTO prediction_fair_snapshots
                 (market_id,guild_id,snapshot_at,fair_pct,reason)
             VALUES (?1,?2,?3,?4,'rollback')",
            params![prediction_id, guild_id, now, current_price],
        )?;
        transaction.commit()?;

        Ok(RollbackResult {
            prediction_id,
            guild_id,
            previous_outcome: outcome,
            total_reversed,
            affected_players,
            lp_pnl: restored_lp_pnl,
            current_price,
        })
    }

    pub fn user_orderbook_stats(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<UserOrderbookStats, PredictionResolutionError> {
        let history = self.player_pnl_history(discord_id, guild_id)?;
        let mut stats = UserOrderbookStats {
            resolved_markets: i64::try_from(history.len())
                .map_err(|_| PredictionResolutionError::ArithmeticOverflow)?,
            ..UserOrderbookStats::default()
        };
        for point in history {
            stats.realized_pnl = stats
                .realized_pnl
                .checked_add(point.delta)
                .ok_or(PredictionResolutionError::ArithmeticOverflow)?;
            if point.delta > 0 {
                stats.wins += 1;
            } else if point.delta < 0 {
                stats.losses += 1;
            }
        }
        Ok(stats)
    }

    pub fn player_pnl_history(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<PnlHistoryPoint>, PredictionResolutionError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(PNL_HISTORY_SQL)?;
        let rows = statement.query_map(params![discord_id, guild_id], |row| {
            let outcome: String = row.get(2)?;
            let yes_contracts: i64 = row.get(3)?;
            let no_contracts: i64 = row.get(4)?;
            let cost: i64 = row.get(5)?;
            let penalty: i64 = row.get(6)?;
            let vanity_tax: i64 = row.get(7)?;
            let settlement_count: i64 = row.get(8)?;
            let credited: i64 = row.get(9)?;
            let won = if outcome == "yes" {
                yes_contracts
            } else {
                no_contracts
            };
            let payout = if settlement_count > 0 {
                credited
            } else {
                won * CONTRACT_VALUE - penalty - vanity_tax
            };
            Ok(PnlHistoryPoint {
                prediction_id: row.get(0)?,
                settle_time: row.get(1)?,
                delta: payout - cost,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// Shared with `predictions_repository::player_pnl_history` so the profile
/// stats and the resolution-side stats can never diverge again.
pub(crate) const PNL_HISTORY_SQL: &str = "
    SELECT p.prediction_id,COALESCE(p.resolved_at,0),p.outcome,
           pp.yes_contracts,pp.no_contracts,
           pp.yes_cost_basis_total+pp.no_cost_basis_total,
           COALESCE(pp.bankruptcy_penalty,0),COALESCE(pp.vanity_tax,0),
           (SELECT COUNT(*) FROM economy_ledger_entries e
            WHERE e.guild_id=p.guild_id AND e.source='prediction_resolution'
              AND e.related_type='prediction'
              AND e.related_id=CAST(p.prediction_id AS TEXT)
              AND e.ledger_id>COALESCE((SELECT MAX(rb.ledger_id)
                  FROM economy_ledger_entries rb
                  WHERE rb.guild_id=p.guild_id
                    AND rb.source='prediction_resolution_rollback'
                    AND rb.related_type='prediction'
                    AND rb.related_id=CAST(p.prediction_id AS TEXT)),0)),
           (SELECT COALESCE(SUM(e.delta),0) FROM economy_ledger_entries e
            WHERE e.guild_id=p.guild_id AND e.account_type='player'
              AND e.account_id=pp.discord_id
              AND e.source IN ('prediction_resolution','vanity_tax',
                               'low_priority_tax')
              AND e.related_type='prediction'
              AND e.related_id=CAST(p.prediction_id AS TEXT)
              AND e.ledger_id>COALESCE((SELECT MAX(rb.ledger_id)
                  FROM economy_ledger_entries rb
                  WHERE rb.guild_id=p.guild_id
                    AND rb.source='prediction_resolution_rollback'
                    AND rb.related_type='prediction'
                    AND rb.related_id=CAST(p.prediction_id AS TEXT)),0))
    FROM prediction_positions pp JOIN predictions p USING(prediction_id)
    WHERE pp.discord_id=?1 AND p.guild_id=?2 AND p.status='resolved'
    ORDER BY p.resolved_at";

#[derive(Clone, Copy, Debug)]
struct PositionRow {
    discord_id: i64,
    yes_contracts: i64,
    yes_cost_basis_total: i64,
    no_contracts: i64,
    no_cost_basis_total: i64,
}

#[derive(Clone, Copy, Debug)]
struct RollbackPosition {
    discord_id: i64,
    yes_contracts: i64,
    no_contracts: i64,
    bankruptcy_penalty: i64,
    vanity_tax: i64,
    low_priority_tax: i64,
}

impl RollbackPosition {
    const fn winning_contracts(self, outcome: ContractSide) -> i64 {
        match outcome {
            ContractSide::Yes => self.yes_contracts,
            ContractSide::No => self.no_contracts,
        }
    }
}

#[derive(Clone, Debug)]
struct LedgerRow {
    ledger_id: i64,
    account_type: String,
    account_id: Option<i64>,
    delta: i64,
    gross_payout: Option<i64>,
    bankruptcy_penalty: Option<i64>,
    base_gross_payout: Option<i64>,
    payout_multiplier_bps: Option<i64>,
}

impl LedgerRow {
    fn player_account_id(&self) -> Option<i64> {
        (self.account_type == "player")
            .then_some(self.account_id)
            .flatten()
    }
}

#[derive(Clone, Copy, Debug)]
struct Reversal {
    discord_id: i64,
    settlement_ledger_id: i64,
    settlement_credit: i64,
    gross_payout: i64,
    bankruptcy_penalty: i64,
    vanity_tax: i64,
    low_priority_tax: i64,
    base_gross_payout: i64,
    payout_multiplier_bps: i64,
}

fn load_positions(
    transaction: &Transaction<'_>,
    prediction_id: i64,
) -> Result<Vec<PositionRow>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT discord_id,yes_contracts,yes_cost_basis_total,
                no_contracts,no_cost_basis_total
         FROM prediction_positions WHERE prediction_id=?1 ORDER BY discord_id",
    )?;
    let rows = statement.query_map([prediction_id], |row| {
        Ok(PositionRow {
            discord_id: row.get(0)?,
            yes_contracts: row.get(1)?,
            yes_cost_basis_total: row.get(2)?,
            no_contracts: row.get(3)?,
            no_cost_basis_total: row.get(4)?,
        })
    })?;
    rows.collect()
}

fn load_rollback_positions(
    transaction: &Transaction<'_>,
    prediction_id: i64,
) -> Result<Vec<RollbackPosition>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT discord_id,yes_contracts,no_contracts,
                COALESCE(bankruptcy_penalty,0),COALESCE(vanity_tax,0),
                COALESCE(low_priority_tax,0)
         FROM prediction_positions WHERE prediction_id=?1",
    )?;
    let rows = statement.query_map([prediction_id], |row| {
        Ok(RollbackPosition {
            discord_id: row.get(0)?,
            yes_contracts: row.get(1)?,
            no_contracts: row.get(2)?,
            bankruptcy_penalty: row.get(3)?,
            vanity_tax: row.get(4)?,
            low_priority_tax: row.get(5)?,
        })
    })?;
    rows.collect()
}

fn latest_rollback_ledger_id(
    transaction: &Transaction<'_>,
    guild_id: i64,
    prediction_id: i64,
) -> Result<i64, rusqlite::Error> {
    transaction.query_row(
        "SELECT COALESCE(MAX(ledger_id),0) FROM economy_ledger_entries
         WHERE guild_id=?1 AND source='prediction_resolution_rollback'
           AND related_type='prediction' AND related_id=?2",
        params![guild_id, prediction_id.to_string()],
        |row| row.get(0),
    )
}

fn load_ledger_rows(
    transaction: &Transaction<'_>,
    guild_id: i64,
    prediction_id: i64,
    source: &str,
    ledger_floor: i64,
) -> Result<Vec<LedgerRow>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT ledger_id,account_type,account_id,delta,
                CASE WHEN json_valid(metadata)
                     THEN CAST(json_extract(metadata,'$.gross_payout') AS INTEGER) END,
                CASE WHEN json_valid(metadata)
                     THEN CAST(json_extract(metadata,'$.bankruptcy_penalty') AS INTEGER) END,
                CASE WHEN json_valid(metadata)
                     THEN CAST(json_extract(metadata,'$.base_gross_payout') AS INTEGER) END,
                CASE WHEN json_valid(metadata)
                     THEN CAST(json_extract(metadata,'$.payout_multiplier_bps') AS INTEGER) END
         FROM economy_ledger_entries
         WHERE guild_id=?1 AND source=?2 AND related_type='prediction'
           AND related_id=?3 AND ledger_id>?4 ORDER BY ledger_id",
    )?;
    let rows = statement.query_map(
        params![guild_id, source, prediction_id.to_string(), ledger_floor],
        |row| {
            Ok(LedgerRow {
                ledger_id: row.get(0)?,
                account_type: row.get(1)?,
                account_id: row.get(2)?,
                delta: row.get(3)?,
                gross_payout: row.get(4)?,
                bankruptcy_penalty: row.get(5)?,
                base_gross_payout: row.get(6)?,
                payout_multiplier_bps: row.get(7)?,
            })
        },
    )?;
    rows.collect()
}

#[derive(Clone, Copy)]
struct LedgeredBalanceMutation<'a> {
    guild_id: i64,
    discord_id: i64,
    delta: i64,
    source: &'a str,
    actor_id: Option<i64>,
    prediction_id: i64,
    reason: &'a str,
    metadata: &'a str,
}

fn update_player_with_context(
    transaction: &Transaction<'_>,
    mutation: LedgeredBalanceMutation<'_>,
) -> Result<(), PredictionResolutionError> {
    set_ledger_context(
        transaction,
        mutation.source,
        mutation.actor_id,
        mutation.prediction_id,
        mutation.reason,
        mutation.metadata,
    )?;
    let changed = transaction.execute(
        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
             updated_at=CURRENT_TIMESTAMP
         WHERE discord_id=?2 AND guild_id=?3",
        params![mutation.delta, mutation.discord_id, mutation.guild_id],
    );
    let cleared = clear_ledger_context(transaction);
    let changed = changed?;
    cleared?;
    if changed != 1 {
        return Err(PredictionResolutionError::WinningPlayerNotFound);
    }
    Ok(())
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: Option<i64>,
    prediction_id: i64,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,?1,?2,'prediction',?3,?4,?5)
         ON CONFLICT(id) DO UPDATE SET source=excluded.source,
             actor_id=excluded.actor_id,related_type=excluded.related_type,
             related_id=excluded.related_id,reason=excluded.reason,
             metadata=excluded.metadata",
        params![
            source,
            actor_id,
            prediction_id.to_string(),
            reason,
            metadata
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context WHERE id=1", [])?;
    Ok(())
}

fn insert_zero_settlement_ledger(
    transaction: &Transaction<'_>,
    guild_id: i64,
    discord_id: i64,
    prediction_id: i64,
    metadata: &str,
    now: i64,
) -> Result<(), PredictionResolutionError> {
    let balance = player_balance(transaction, guild_id, discord_id)?;
    transaction.execute(
        "INSERT INTO economy_ledger_entries
             (guild_id,account_type,account_id,delta,balance_before,balance_after,
              source,related_type,related_id,reason,metadata,created_at)
         VALUES (?1,'player',?2,0,?3,?3,'prediction_resolution','prediction',?4,
                 'prediction resolution payout',?5,?6)",
        params![
            guild_id,
            discord_id,
            balance,
            prediction_id.to_string(),
            metadata,
            now
        ],
    )?;
    Ok(())
}

fn insert_zero_rollback_ledger(
    transaction: &Transaction<'_>,
    guild_id: i64,
    discord_id: i64,
    prediction_id: i64,
    actor_id: Option<i64>,
    metadata: &str,
    now: i64,
) -> Result<(), PredictionResolutionError> {
    let balance = player_balance(transaction, guild_id, discord_id)?;
    transaction.execute(
        "INSERT INTO economy_ledger_entries
             (guild_id,account_type,account_id,delta,balance_before,balance_after,
              source,actor_id,related_type,related_id,reason,metadata,created_at)
         VALUES (?1,'player',?2,0,?3,?3,'prediction_resolution_rollback',?4,
                 'prediction',?5,'prediction resolution rollback',?6,?7)",
        params![
            guild_id,
            discord_id,
            balance,
            actor_id,
            prediction_id.to_string(),
            metadata,
            now
        ],
    )?;
    Ok(())
}

fn player_balance(
    transaction: &Transaction<'_>,
    guild_id: i64,
    discord_id: i64,
) -> Result<i64, PredictionResolutionError> {
    transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(PredictionResolutionError::WinningPlayerNotFound)
}

#[allow(clippy::too_many_arguments)]
fn settlement_metadata(
    outcome: ContractSide,
    base_gross_payout: i64,
    gross_payout: i64,
    payout_multiplier_bps: i64,
    bankruptcy_penalty: i64,
    vanity_tax: i64,
    low_priority_tax: i64,
    winning_contracts: i64,
) -> String {
    format!(
        "{{\"outcome\":\"{}\",\"base_gross_payout\":{base_gross_payout},\
         \"gross_payout\":{gross_payout},\"payout_multiplier_bps\":{payout_multiplier_bps},\
         \"bankruptcy_penalty\":{bankruptcy_penalty},\"vanity_tax\":{vanity_tax},\
         \"low_priority_tax\":{low_priority_tax},\
         \"winning_contracts\":{winning_contracts}}}",
        side_name(outcome)
    )
}

fn validate_adjustments(
    adjustments: &SettlementAdjustments,
) -> Result<(), PredictionResolutionError> {
    if adjustments.contract_value < 0
        || adjustments.payout_multiplier_bps < 0
        || adjustments.vanity_tax_bps < 0
        || adjustments.low_priority_tax_bps < 0
        || adjustments
            .bankruptcy_penalty_bps_per_game
            .is_some_and(|bps| !(0..=BASIS_POINTS).contains(&bps))
    {
        return Err(PredictionResolutionError::InvalidAdjustments);
    }
    Ok(())
}

fn validate_levels(levels: &[NewLevel]) -> Result<(), PredictionResolutionError> {
    if levels.iter().any(|level| level.price < 0 || level.size < 0) {
        return Err(PredictionResolutionError::InvalidLevel);
    }
    Ok(())
}

fn multiply_bps_floor(value: i64, bps: i64) -> Result<i64, PredictionResolutionError> {
    let scaled = i128::from(value)
        .checked_mul(i128::from(bps))
        .ok_or(PredictionResolutionError::ArithmeticOverflow)?
        / i128::from(BASIS_POINTS);
    i64::try_from(scaled).map_err(|_| PredictionResolutionError::ArithmeticOverflow)
}

/// Truncate `gross_profit * withheld_rate` in IEEE doubles so prediction
/// settlement withholds the same coins as every other profit surface, which
/// truncate the f64 product rather than flooring integer basis points.
fn bankruptcy_withholding(
    gross_profit: i64,
    bps_per_game: i64,
    penalty_games_remaining: i64,
) -> Result<i64, PredictionResolutionError> {
    let rate_per_game = bps_per_game as f64 / BASIS_POINTS as f64;
    let product = gross_profit as f64
        * cama_domain::bankruptcy::withheld_rate(rate_per_game, penalty_games_remaining);
    if !product.is_finite() {
        return Err(PredictionResolutionError::ArithmeticOverflow);
    }
    let truncated = product.trunc();
    if truncated < i64::MIN as f64 || truncated >= 9_223_372_036_854_775_808.0 {
        return Err(PredictionResolutionError::ArithmeticOverflow);
    }
    Ok(truncated as i64)
}

/// Python parity (`prediction_repository.py::resolve_market`):
/// `round(base_payout * payout_multiplier)` evaluates the product in IEEE
/// doubles and rounds half-to-even. An exact rational computation rounds
/// differently whenever the true product sits on a half boundary the double
/// product misses (300 × 1.005 must pay 301, not 302).
fn multiply_bps_round_ties_even(value: i64, bps: i64) -> Result<i64, PredictionResolutionError> {
    let product = value as f64 * (bps as f64 / BASIS_POINTS as f64);
    if !product.is_finite() {
        return Err(PredictionResolutionError::ArithmeticOverflow);
    }
    let rounded = product.round_ties_even();
    if rounded < i64::MIN as f64 || rounded >= 9_223_372_036_854_775_808.0 {
        return Err(PredictionResolutionError::ArithmeticOverflow);
    }
    Ok(rounded as i64)
}

const fn side_name(side: ContractSide) -> &'static str {
    match side {
        ContractSide::Yes => "yes",
        ContractSide::No => "no",
    }
}

fn parse_side(value: Option<&str>) -> Result<ContractSide, PredictionResolutionError> {
    match value {
        Some("yes") => Ok(ContractSide::Yes),
        Some("no") => Ok(ContractSide::No),
        _ => Err(PredictionResolutionError::MissingOutcome),
    }
}

fn unix_timestamp() -> Result<i64, PredictionResolutionError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PredictionResolutionError::ClockBeforeUnixEpoch)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| PredictionResolutionError::ArithmeticOverflow)
}

#[cfg(test)]
#[path = "prediction_resolution/tests.rs"]
mod tests;
