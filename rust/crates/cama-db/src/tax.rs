//! Existing-schema persistence for the Tax Man audit and enforcement surface.
//!
//! Read models intentionally mirror the Python `TaxService` payloads. Fine
//! enforcement acquires `BEGIN IMMEDIATE` and re-derives every obligation
//! inside that lock before moving wallet value to the reserve, so the wallet,
//! reserve, trigger-written ledger rows, and cooldown are one commit.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxLedgerEntry {
    pub ledger_id: i64,
    pub guild_id: i64,
    pub account_type: String,
    pub account_id: Option<i64>,
    pub delta: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub source: String,
    pub actor_id: Option<i64>,
    pub related_type: Option<String>,
    pub related_id: Option<String>,
    pub reason: Option<String>,
    pub metadata: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxLedgerSourceTotal {
    pub source: String,
    pub entry_count: i64,
    pub net_delta: i64,
    pub inflow: i64,
    pub outflow: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaxPredictionSummary {
    pub open_markets: i64,
    pub holder_count: i64,
    pub yes_contracts: i64,
    pub no_contracts: i64,
    pub cost_basis: i64,
    pub expected_payout: i64,
    pub ev_to_holders: i64,
    pub yes_liability: i64,
    pub no_liability: i64,
    pub worst_case_payout: i64,
    pub lp_pnl: i64,
    pub book_contracts: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxPredictionMarket {
    pub prediction_id: i64,
    pub question: String,
    pub status: String,
    pub current_price: i64,
    pub holder_count: i64,
    pub yes_contracts: i64,
    pub no_contracts: i64,
    pub yes_cost_basis: i64,
    pub no_cost_basis: i64,
    pub cost_basis: i64,
    pub expected_payout: i64,
    pub ev_to_holders: i64,
    pub yes_liability: i64,
    pub no_liability: i64,
    pub worst_case_payout: i64,
    pub lp_pnl: i64,
    pub top_yes_ask: Option<i64>,
    pub top_yes_bid: Option<i64>,
    pub book_contracts: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaxPredictionMarketExposure {
    pub summary: TaxPredictionSummary,
    pub markets: Vec<TaxPredictionMarket>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaxPlayerPredictionSummary {
    pub markets: i64,
    pub yes_contracts: i64,
    pub no_contracts: i64,
    pub cost_basis: i64,
    pub expected_payout: i64,
    pub ev: i64,
    pub max_payout: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxPlayerPredictionPosition {
    pub prediction_id: i64,
    pub question: String,
    pub status: String,
    pub current_price: i64,
    pub yes_contracts: i64,
    pub no_contracts: i64,
    pub yes_cost_basis: i64,
    pub no_cost_basis: i64,
    pub cost_basis: i64,
    pub expected_payout: i64,
    pub ev: i64,
    pub yes_max_payout: i64,
    pub no_max_payout: i64,
    pub max_payout: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaxPlayerPredictionExposure {
    pub summary: TaxPlayerPredictionSummary,
    pub positions: Vec<TaxPlayerPredictionPosition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxGuildSnapshot {
    pub guild_id: i64,
    pub players: i64,
    pub total_balance: i64,
    pub positive_balance: i64,
    pub visible_debt: i64,
    pub broke_players: i64,
    pub nonprofit_available: i64,
    pub nonprofit_reserved: i64,
    pub loan_principal: i64,
    pub loan_fee: i64,
    pub loan_borrowers: i64,
    pub dark_bargain_count: i64,
    pub dark_bargain_due: i64,
    pub pending_bet_effective_stake: i64,
    pub pending_bet_count: i64,
    pub open_prediction_markets: i64,
    pub prediction_exposure: TaxPredictionMarketExposure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxPlayerSnapshot {
    pub discord_id: i64,
    pub guild_id: i64,
    pub name: String,
    pub balance: i64,
    pub visible_debt: i64,
    pub loan_principal: i64,
    pub loan_fee: i64,
    pub loan_total: i64,
    pub total_loans_taken: i64,
    pub total_fees_paid: i64,
    pub bankruptcy_count: i64,
    pub penalty_games_remaining: i64,
    pub dark_bargain_count: i64,
    pub dark_bargain_due: i64,
    pub prediction_exposure: TaxPlayerPredictionExposure,
    pub prediction_cost_basis: i64,
    pub effective_obligations: i64,
    pub recent_ledger_total: i64,
    pub recent_ledger_limit: usize,
    pub recent_ledger_offset: usize,
    pub recent_ledger: Vec<TaxLedgerEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxFineRequest {
    pub discord_id: i64,
    pub guild_id: i64,
    pub amount: i64,
    /// Caller-audited cap, re-clamped against obligations under the write lock.
    pub max_amount: i64,
    pub actor_id: i64,
    pub reason: Option<String>,
    pub cooldown_seconds: i64,
    pub now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaxFineOutcome {
    Ok {
        requested_amount: i64,
        applied_amount: i64,
        outstanding_obligations: i64,
        balance_before: i64,
        balance_after: i64,
        next_fine_at: i64,
    },
    Cooldown {
        next_fine_at: i64,
    },
    NoOutstandingObligations,
    TargetNotRegistered,
    InvalidAmount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaxBankruptcyAction {
    Add { games: i64 },
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaxBankruptcyRequest {
    pub discord_id: i64,
    pub guild_id: i64,
    pub action: TaxBankruptcyAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaxBankruptcyOutcome {
    Added {
        games: i64,
        previous_games: i64,
        penalty_games_remaining: i64,
    },
    Removed {
        previous_games: i64,
        penalty_games_remaining: i64,
    },
    TargetNotRegistered,
    InvalidGames,
}

#[derive(Debug, Error)]
pub enum TaxRepositoryError {
    #[error("Tax Man arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct TaxRepository {
    path: PathBuf,
    prediction_contract_value: i64,
    prediction_initial_fair_default: i64,
}

impl TaxRepository {
    #[must_use]
    pub fn new(
        path: impl AsRef<Path>,
        prediction_contract_value: i64,
        prediction_initial_fair_default: i64,
    ) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            prediction_contract_value,
            prediction_initial_fair_default,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn count_ledger_entries(
        &self,
        guild_id: i64,
        user_id: Option<i64>,
    ) -> Result<usize, TaxRepositoryError> {
        let connection = self.connection()?;
        let count = if let Some(user_id) = user_id {
            connection.query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_type='player' AND account_id=?2",
                params![guild_id, user_id],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            connection.query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries WHERE guild_id=?1",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )?
        };
        Ok(usize::try_from(count.max(0)).unwrap_or(usize::MAX))
    }

    pub fn recent_ledger(
        &self,
        guild_id: i64,
        limit: usize,
        offset: usize,
        user_id: Option<i64>,
    ) -> Result<Vec<TaxLedgerEntry>, TaxRepositoryError> {
        let connection = self.connection()?;
        let limit = i64::try_from(limit.clamp(1, 100)).unwrap_or(100);
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let mut entries = Vec::new();
        if let Some(user_id) = user_id {
            let mut statement = connection.prepare(
                "SELECT ledger_id,guild_id,account_type,account_id,delta,
                        balance_before,balance_after,source,actor_id,
                        related_type,related_id,reason,metadata,created_at
                 FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_type='player' AND account_id=?2
                 ORDER BY created_at DESC,ledger_id DESC LIMIT ?3 OFFSET ?4",
            )?;
            let rows =
                statement.query_map(params![guild_id, user_id, limit, offset], ledger_row)?;
            for row in rows {
                entries.push(row?);
            }
        } else {
            let mut statement = connection.prepare(
                "SELECT ledger_id,guild_id,account_type,account_id,delta,
                        balance_before,balance_after,source,actor_id,
                        related_type,related_id,reason,metadata,created_at
                 FROM economy_ledger_entries WHERE guild_id=?1
                 ORDER BY created_at DESC,ledger_id DESC LIMIT ?2 OFFSET ?3",
            )?;
            let rows = statement.query_map(params![guild_id, limit, offset], ledger_row)?;
            for row in rows {
                entries.push(row?);
            }
        }
        Ok(entries)
    }

    pub fn source_totals(
        &self,
        guild_id: i64,
        limit: usize,
    ) -> Result<Vec<TaxLedgerSourceTotal>, TaxRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT source,COUNT(*),COALESCE(SUM(delta),0),
                    COALESCE(SUM(CASE WHEN delta>0 THEN delta ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN delta<0 THEN -delta ELSE 0 END),0)
             FROM economy_ledger_entries WHERE guild_id=?1
             GROUP BY source ORDER BY COUNT(*) DESC,source ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![guild_id, i64::try_from(limit.clamp(1, 100)).unwrap_or(100)],
            |row| {
                Ok(TaxLedgerSourceTotal {
                    source: row.get(0)?,
                    entry_count: row.get(1)?,
                    net_delta: row.get(2)?,
                    inflow: row.get(3)?,
                    outflow: row.get(4)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn guild_snapshot(
        &self,
        guild_id: i64,
        now: i64,
    ) -> Result<TaxGuildSnapshot, TaxRepositoryError> {
        let connection = self.connection()?;
        let players = connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(jopacoin_balance),0),
                    COALESCE(SUM(CASE WHEN jopacoin_balance>0 THEN jopacoin_balance ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN jopacoin_balance<0 THEN -jopacoin_balance ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN jopacoin_balance<=0 THEN 1 ELSE 0 END),0)
             FROM players WHERE guild_id=?1",
            [guild_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let nonprofit_available = connection
            .query_row(
                "SELECT COALESCE(total_collected,0) FROM nonprofit_fund WHERE guild_id=?1",
                [guild_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let loans = connection.query_row(
            "SELECT COALESCE(SUM(outstanding_principal),0),
                    COALESCE(SUM(outstanding_fee),0),
                    COUNT(CASE WHEN outstanding_principal>0 THEN 1 END)
             FROM loan_state WHERE guild_id=?1",
            [guild_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let nonprofit_reserved = connection.query_row(
            "SELECT COALESCE(SUM(fund_amount),0) FROM disburse_proposals
             WHERE guild_id=?1 AND status='active'",
            [guild_id],
            |row| row.get(0),
        )?;
        let pending_bets = connection.query_row(
            "SELECT COALESCE(SUM(amount*COALESCE(leverage,1)),0),COUNT(*)
             FROM bets WHERE guild_id=?1 AND match_id IS NULL",
            [guild_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (dark_bargain_count, dark_bargain_due) =
            active_dark_bargains(&connection, guild_id, None, now)?;
        let prediction_exposure = self.prediction_market_exposure_with(&connection, guild_id, 5)?;
        Ok(TaxGuildSnapshot {
            guild_id,
            players: players.0,
            total_balance: players.1,
            positive_balance: players.2,
            visible_debt: players.3,
            broke_players: players.4,
            nonprofit_available,
            nonprofit_reserved,
            loan_principal: loans.0,
            loan_fee: loans.1,
            loan_borrowers: loans.2,
            dark_bargain_count,
            dark_bargain_due,
            pending_bet_effective_stake: pending_bets.0,
            pending_bet_count: pending_bets.1,
            open_prediction_markets: prediction_exposure.summary.open_markets,
            prediction_exposure,
        })
    }

    pub fn player_snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
        ledger_limit: usize,
        ledger_offset: usize,
        now: i64,
    ) -> Result<Option<TaxPlayerSnapshot>, TaxRepositoryError> {
        let connection = self.connection()?;
        let player = connection
            .query_row(
                "SELECT discord_username,COALESCE(jopacoin_balance,0)
                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((name, balance)) = player else {
            return Ok(None);
        };
        let loan = connection
            .query_row(
                "SELECT COALESCE(outstanding_principal,0),COALESCE(outstanding_fee,0),
                        COALESCE(total_loans_taken,0),COALESCE(total_fees_paid,0)
                 FROM loan_state WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .unwrap_or((0_i64, 0_i64, 0_i64, 0_i64));
        let bankruptcy = connection
            .query_row(
                "SELECT COALESCE(bankruptcy_count,0),COALESCE(penalty_games_remaining,0)
                 FROM bankruptcy_state WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or((0_i64, 0_i64));
        let (dark_bargain_count, dark_bargain_due) =
            active_dark_bargains(&connection, guild_id, Some(discord_id), now)?;
        let prediction_exposure =
            self.player_prediction_exposure_with(&connection, discord_id, guild_id)?;
        let prediction_cost_basis = prediction_exposure.summary.cost_basis;
        let visible_debt = balance.saturating_neg().max(0);
        let loan_total = loan.0.saturating_add(loan.1);
        let effective_obligations = visible_debt
            .saturating_add(loan_total)
            .saturating_add(dark_bargain_due)
            .saturating_add(prediction_cost_basis);
        let recent_ledger_total = self.count_ledger_entries(guild_id, Some(discord_id))?;
        let recent_ledger =
            self.recent_ledger(guild_id, ledger_limit, ledger_offset, Some(discord_id))?;
        Ok(Some(TaxPlayerSnapshot {
            discord_id,
            guild_id,
            name,
            balance,
            visible_debt,
            loan_principal: loan.0,
            loan_fee: loan.1,
            loan_total,
            total_loans_taken: loan.2,
            total_fees_paid: loan.3,
            bankruptcy_count: bankruptcy.0,
            penalty_games_remaining: bankruptcy.1,
            dark_bargain_count,
            dark_bargain_due,
            prediction_exposure,
            prediction_cost_basis,
            effective_obligations,
            recent_ledger_total: i64::try_from(recent_ledger_total).unwrap_or(i64::MAX),
            recent_ledger_limit: ledger_limit,
            recent_ledger_offset: ledger_offset,
            recent_ledger,
        }))
    }

    pub fn levy_fine_atomic(
        &self,
        request: &TaxFineRequest,
    ) -> Result<TaxFineOutcome, TaxRepositoryError> {
        if request.amount <= 0 {
            return Ok(TaxFineOutcome::InvalidAmount);
        }
        if request.max_amount <= 0 {
            return Ok(TaxFineOutcome::NoOutstandingObligations);
        }
        let cooldown_seconds = request.cooldown_seconds.max(0);
        let reason = request
            .reason
            .as_deref()
            .map(normalize_whitespace)
            .filter(|reason| !reason.is_empty());
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![request.discord_id, request.guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(balance_before) = balance else {
            transaction.commit()?;
            return Ok(TaxFineOutcome::TargetNotRegistered);
        };
        let last_fined_at = transaction
            .query_row(
                "SELECT last_fined_at FROM tax_fine_cooldowns
                 WHERE discord_id=?1 AND guild_id=?2",
                params![request.discord_id, request.guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(last_fined_at) = last_fined_at
            && cooldown_seconds > 0
        {
            let next_fine_at = last_fined_at.saturating_add(cooldown_seconds);
            if request.now < next_fine_at {
                transaction.commit()?;
                return Ok(TaxFineOutcome::Cooldown { next_fine_at });
            }
        }
        let fresh_obligations = obligations_in_transaction(
            &transaction,
            request.discord_id,
            request.guild_id,
            balance_before,
            request.now,
        )?;
        let obligations = request.max_amount.min(fresh_obligations);
        if obligations <= 0 {
            transaction.commit()?;
            return Ok(TaxFineOutcome::NoOutstandingObligations);
        }
        let applied_amount = request.amount.min(obligations);
        let balance_after = balance_before
            .checked_sub(applied_amount)
            .ok_or(TaxRepositoryError::ArithmeticOverflow)?;
        let ledger_reason = reason.as_deref().map_or_else(
            || "tax fine".to_owned(),
            |reason| format!("tax fine: {reason}"),
        );
        // Match Python's insertion-ordered `json.dumps` representation because
        // the trigger persists this byte-for-byte into both ledger rows.
        let metadata = format!(
            "{{\"requested_amount\": {}, \"applied_amount\": {}, \"outstanding_obligations\": {}, \"cooldown_seconds\": {cooldown_seconds}}}",
            request.amount, applied_amount, obligations,
        );
        set_ledger_context(
            &transaction,
            request.actor_id,
            request.discord_id,
            &ledger_reason,
            &metadata,
        )?;
        transaction.execute(
            "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![balance_after, request.discord_id, request.guild_id],
        )?;
        transaction.execute(
            "UPDATE players SET lowest_balance_ever=?1
             WHERE discord_id=?2 AND guild_id=?3
               AND (lowest_balance_ever IS NULL OR ?1<lowest_balance_ever)",
            params![balance_after, request.discord_id, request.guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        set_ledger_context(
            &transaction,
            request.actor_id,
            request.discord_id,
            &format!("{ledger_reason} reserve credit"),
            &metadata,
        )?;
        transaction.execute(
            "INSERT INTO nonprofit_fund(guild_id,total_collected,updated_at)
             VALUES (?1,?2,CURRENT_TIMESTAMP)
             ON CONFLICT(guild_id) DO UPDATE SET
                total_collected=total_collected+excluded.total_collected,
                updated_at=CURRENT_TIMESTAMP",
            params![request.guild_id, applied_amount],
        )?;
        clear_ledger_context(&transaction)?;
        transaction.execute(
            "INSERT INTO tax_fine_cooldowns(
                 discord_id,guild_id,last_fined_at,last_amount,last_actor_id,last_reason,updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                 last_fined_at=excluded.last_fined_at,last_amount=excluded.last_amount,
                 last_actor_id=excluded.last_actor_id,last_reason=excluded.last_reason,
                 updated_at=CURRENT_TIMESTAMP",
            params![
                request.discord_id,
                request.guild_id,
                request.now,
                applied_amount,
                request.actor_id,
                reason,
            ],
        )?;
        transaction.commit()?;
        Ok(TaxFineOutcome::Ok {
            requested_amount: request.amount,
            applied_amount,
            outstanding_obligations: obligations,
            balance_before,
            balance_after,
            next_fine_at: request.now.saturating_add(cooldown_seconds),
        })
    }

    pub fn reset_fine_cooldown_atomic(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<bool>, TaxRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
            params![discord_id, guild_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            transaction.commit()?;
            return Ok(None);
        }
        let changed = transaction.execute(
            "DELETE FROM tax_fine_cooldowns WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )? > 0;
        transaction.commit()?;
        Ok(Some(changed))
    }

    pub fn change_bankruptcy_modifier_atomic(
        &self,
        request: TaxBankruptcyRequest,
    ) -> Result<TaxBankruptcyOutcome, TaxRepositoryError> {
        if matches!(request.action, TaxBankruptcyAction::Add { games } if games <= 0) {
            return Ok(TaxBankruptcyOutcome::InvalidGames);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
            params![request.discord_id, request.guild_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            transaction.commit()?;
            return Ok(TaxBankruptcyOutcome::TargetNotRegistered);
        }
        let previous_games = transaction
            .query_row(
                "SELECT COALESCE(penalty_games_remaining,0) FROM bankruptcy_state
                 WHERE discord_id=?1 AND guild_id=?2",
                params![request.discord_id, request.guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let outcome = match request.action {
            TaxBankruptcyAction::Add { games } => {
                let remaining = previous_games
                    .checked_add(games)
                    .ok_or(TaxRepositoryError::ArithmeticOverflow)?;
                transaction.execute(
                    "INSERT INTO bankruptcy_state(
                         discord_id,guild_id,last_bankruptcy_at,
                         penalty_games_remaining,bankruptcy_count,updated_at
                     ) VALUES (?1,?2,NULL,?3,0,CURRENT_TIMESTAMP)
                     ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                         penalty_games_remaining=?3,updated_at=CURRENT_TIMESTAMP",
                    params![request.discord_id, request.guild_id, remaining],
                )?;
                TaxBankruptcyOutcome::Added {
                    games,
                    previous_games,
                    penalty_games_remaining: remaining,
                }
            }
            TaxBankruptcyAction::Remove => {
                transaction.execute(
                    "INSERT INTO bankruptcy_state(
                         discord_id,guild_id,last_bankruptcy_at,
                         penalty_games_remaining,bankruptcy_count,updated_at
                     ) VALUES (?1,?2,NULL,0,0,CURRENT_TIMESTAMP)
                     ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                         penalty_games_remaining=0,updated_at=CURRENT_TIMESTAMP",
                    params![request.discord_id, request.guild_id],
                )?;
                TaxBankruptcyOutcome::Removed {
                    previous_games,
                    penalty_games_remaining: 0,
                }
            }
        };
        transaction.commit()?;
        Ok(outcome)
    }

    fn prediction_market_exposure_with(
        &self,
        connection: &Connection,
        guild_id: i64,
        limit: usize,
    ) -> Result<TaxPredictionMarketExposure, TaxRepositoryError> {
        let mut statement = connection.prepare(
            "WITH pos AS (
                 SELECT prediction_id,COUNT(DISTINCT discord_id) AS holder_count,
                        COALESCE(SUM(yes_contracts),0) AS yes_contracts,
                        COALESCE(SUM(no_contracts),0) AS no_contracts,
                        COALESCE(SUM(yes_cost_basis_total),0) AS yes_cost_basis,
                        COALESCE(SUM(no_cost_basis_total),0) AS no_cost_basis
                 FROM prediction_positions GROUP BY prediction_id
             ), lvl AS (
                 SELECT prediction_id,
                        MIN(CASE WHEN side='yes_ask' THEN price END) AS top_yes_ask,
                        MAX(CASE WHEN side='yes_bid' THEN price END) AS top_yes_bid,
                        COALESCE(SUM(remaining_size),0) AS book_contracts
                 FROM prediction_levels WHERE remaining_size>0 GROUP BY prediction_id
             )
             SELECT p.prediction_id,p.question,p.status,
                    COALESCE(p.current_price,p.initial_fair,?1),COALESCE(p.lp_pnl,0),
                    COALESCE(pos.holder_count,0),COALESCE(pos.yes_contracts,0),
                    COALESCE(pos.no_contracts,0),COALESCE(pos.yes_cost_basis,0),
                    COALESCE(pos.no_cost_basis,0),lvl.top_yes_ask,lvl.top_yes_bid,
                    COALESCE(lvl.book_contracts,0)
             FROM predictions p LEFT JOIN pos ON pos.prediction_id=p.prediction_id
             LEFT JOIN lvl ON lvl.prediction_id=p.prediction_id
             WHERE p.guild_id=?2 AND p.status IN ('open','locked')
             ORDER BY p.created_at DESC,p.prediction_id DESC",
        )?;
        let raw = statement
            .query_map(
                params![self.prediction_initial_fair_default, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, Option<i64>>(11)?,
                        row.get::<_, i64>(12)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut markets = Vec::with_capacity(raw.len());
        for row in raw {
            let yes_liability = row.6.saturating_mul(self.prediction_contract_value);
            let no_liability = row.7.saturating_mul(self.prediction_contract_value);
            let expected_payout =
                expected_payout(row.6, row.7, self.prediction_contract_value, row.3);
            let cost_basis = row.8.saturating_add(row.9);
            markets.push(TaxPredictionMarket {
                prediction_id: row.0,
                question: row.1,
                status: row.2,
                current_price: row.3,
                lp_pnl: row.4,
                holder_count: row.5,
                yes_contracts: row.6,
                no_contracts: row.7,
                yes_cost_basis: row.8,
                no_cost_basis: row.9,
                cost_basis,
                expected_payout,
                ev_to_holders: expected_payout.saturating_sub(cost_basis),
                yes_liability,
                no_liability,
                worst_case_payout: yes_liability.max(no_liability),
                top_yes_ask: row.10,
                top_yes_bid: row.11,
                book_contracts: row.12,
            });
        }
        let summary = markets.iter().fold(
            TaxPredictionSummary {
                open_markets: i64::try_from(markets.len()).unwrap_or(i64::MAX),
                ..TaxPredictionSummary::default()
            },
            |mut total, market| {
                total.holder_count = total.holder_count.saturating_add(market.holder_count);
                total.yes_contracts = total.yes_contracts.saturating_add(market.yes_contracts);
                total.no_contracts = total.no_contracts.saturating_add(market.no_contracts);
                total.cost_basis = total.cost_basis.saturating_add(market.cost_basis);
                total.expected_payout =
                    total.expected_payout.saturating_add(market.expected_payout);
                total.ev_to_holders = total.ev_to_holders.saturating_add(market.ev_to_holders);
                total.yes_liability = total.yes_liability.saturating_add(market.yes_liability);
                total.no_liability = total.no_liability.saturating_add(market.no_liability);
                total.worst_case_payout = total
                    .worst_case_payout
                    .saturating_add(market.worst_case_payout);
                total.lp_pnl = total.lp_pnl.saturating_add(market.lp_pnl);
                total.book_contracts = total.book_contracts.saturating_add(market.book_contracts);
                total
            },
        );
        markets.truncate(limit.clamp(1, 25));
        Ok(TaxPredictionMarketExposure { summary, markets })
    }

    fn player_prediction_exposure_with(
        &self,
        connection: &Connection,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<TaxPlayerPredictionExposure, TaxRepositoryError> {
        let mut statement = connection.prepare(
            "SELECT p.prediction_id,p.question,p.status,
                    COALESCE(p.current_price,p.initial_fair,?1),
                    pp.yes_contracts,pp.yes_cost_basis_total,
                    pp.no_contracts,pp.no_cost_basis_total
             FROM prediction_positions pp
             JOIN predictions p ON p.prediction_id=pp.prediction_id
             WHERE pp.discord_id=?2 AND p.guild_id=?3
               AND p.status IN ('open','locked')
               AND (pp.yes_contracts>0 OR pp.no_contracts>0)
             ORDER BY p.created_at DESC,p.prediction_id DESC",
        )?;
        let raw = statement
            .query_map(
                params![self.prediction_initial_fair_default, discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let positions = raw
            .into_iter()
            .map(|row| {
                let expected =
                    expected_player_payout(row.4, row.6, self.prediction_contract_value, row.3);
                let cost_basis = row.5.saturating_add(row.7);
                let yes_max = row.4.saturating_mul(self.prediction_contract_value);
                let no_max = row.6.saturating_mul(self.prediction_contract_value);
                TaxPlayerPredictionPosition {
                    prediction_id: row.0,
                    question: row.1,
                    status: row.2,
                    current_price: row.3,
                    yes_contracts: row.4,
                    yes_cost_basis: row.5,
                    no_contracts: row.6,
                    no_cost_basis: row.7,
                    cost_basis,
                    expected_payout: expected,
                    ev: expected.saturating_sub(cost_basis),
                    yes_max_payout: yes_max,
                    no_max_payout: no_max,
                    max_payout: yes_max.saturating_add(no_max),
                }
            })
            .collect::<Vec<_>>();
        let summary = positions.iter().fold(
            TaxPlayerPredictionSummary {
                markets: i64::try_from(positions.len()).unwrap_or(i64::MAX),
                ..TaxPlayerPredictionSummary::default()
            },
            |mut total, position| {
                total.yes_contracts = total.yes_contracts.saturating_add(position.yes_contracts);
                total.no_contracts = total.no_contracts.saturating_add(position.no_contracts);
                total.cost_basis = total.cost_basis.saturating_add(position.cost_basis);
                total.expected_payout = total
                    .expected_payout
                    .saturating_add(position.expected_payout);
                total.ev = total.ev.saturating_add(position.ev);
                total.max_payout = total.max_payout.saturating_add(position.max_payout);
                total
            },
        );
        Ok(TaxPlayerPredictionExposure { summary, positions })
    }
}

fn ledger_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaxLedgerEntry> {
    Ok(TaxLedgerEntry {
        ledger_id: row.get(0)?,
        guild_id: row.get(1)?,
        account_type: row.get(2)?,
        account_id: row.get(3)?,
        delta: row.get(4)?,
        balance_before: row.get(5)?,
        balance_after: row.get(6)?,
        source: row.get(7)?,
        actor_id: row.get(8)?,
        related_type: row.get(9)?,
        related_id: row.get(10)?,
        reason: row.get(11)?,
        metadata: row.get(12)?,
        created_at: row.get(13)?,
    })
}

fn active_dark_bargains(
    connection: &Connection,
    guild_id: i64,
    discord_id: Option<i64>,
    now: i64,
) -> Result<(i64, i64), rusqlite::Error> {
    let mut data = Vec::new();
    if let Some(discord_id) = discord_id {
        let mut statement = connection.prepare(
            "SELECT data FROM manashop_buffs
             WHERE guild_id=?1 AND discord_id=?2 AND buff_type='dark_bargain'
               AND triggered=0 AND expires_at>?3 ORDER BY expires_at ASC",
        )?;
        let rows = statement.query_map(params![guild_id, discord_id, now], |row| row.get(0))?;
        for row in rows {
            data.push(row?);
        }
    } else {
        let mut statement = connection.prepare(
            "SELECT data FROM manashop_buffs
             WHERE guild_id=?1 AND buff_type='dark_bargain'
               AND triggered=0 AND expires_at>?2 ORDER BY expires_at ASC",
        )?;
        let rows = statement.query_map(params![guild_id, now], |row| row.get(0))?;
        for row in rows {
            data.push(row?);
        }
    }
    let due = data.iter().fold(0_i64, |total, raw: &String| {
        let amount = serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value.get("amount_due").and_then(Value::as_i64))
            .unwrap_or(0);
        total.saturating_add(amount)
    });
    Ok((i64::try_from(data.len()).unwrap_or(i64::MAX), due))
}

fn obligations_in_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    balance: i64,
    now: i64,
) -> Result<i64, rusqlite::Error> {
    let visible_debt = balance.saturating_neg().max(0);
    let loan = transaction
        .query_row(
            "SELECT COALESCE(outstanding_principal,0)+COALESCE(outstanding_fee,0)
             FROM loan_state WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    let (_, dark_bargain_due) = active_dark_bargains(transaction, guild_id, Some(discord_id), now)?;
    let prediction = transaction.query_row(
        "SELECT COALESCE(SUM(COALESCE(pp.yes_cost_basis_total,0)
                                  +COALESCE(pp.no_cost_basis_total,0)),0)
         FROM prediction_positions pp
         JOIN predictions p ON p.prediction_id=pp.prediction_id
         WHERE pp.discord_id=?1 AND p.guild_id=?2
           AND p.status IN ('open','locked')
           AND (pp.yes_contracts>0 OR pp.no_contracts>0)",
        params![discord_id, guild_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(visible_debt
        .saturating_add(loan)
        .saturating_add(dark_bargain_due)
        .saturating_add(prediction))
}

fn expected_payout(yes: i64, no: i64, contract_value: i64, price: i64) -> i64 {
    yes.saturating_mul(contract_value)
        .saturating_mul(price)
        .saturating_add(
            no.saturating_mul(contract_value)
                .saturating_mul(100_i64.saturating_sub(price)),
        )
        / 100
}

fn expected_player_payout(yes: i64, no: i64, contract_value: i64, price: i64) -> i64 {
    let yes_expected = yes.saturating_mul(contract_value).saturating_mul(price) / 100;
    let no_expected = no
        .saturating_mul(contract_value)
        .saturating_mul(100_i64.saturating_sub(price))
        / 100;
    yes_expected.saturating_add(no_expected)
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    related_id: i64,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context(
             id,source,actor_id,related_type,related_id,reason,metadata
         ) VALUES (1,'tax_fine',?1,'tax_fine',?2,?3,?4)",
        params![actor_id, related_id.to_string(), reason, metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[path = "tax/tests.rs"]
mod tests;
