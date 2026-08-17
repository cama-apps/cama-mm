//! Existing-schema SQLite persistence for Python's order-book predictions.
//!
//! Python remains the only migration authority during the side-by-side
//! rewrite. This repository never creates or alters production schema. Every
//! multi-row mutation uses `BEGIN IMMEDIATE`, matching Python's single-writer
//! boundary so wallet checks, book fills, positions, trades, and market state
//! commit together on the shared SQLite database.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

pub const CONTRACT_VALUE: i64 = 10;
pub const PRICE_LOW: i64 = 5;
pub const PRICE_HIGH: i64 = 95;
pub const MAX_CONTRACTS_PER_TRADE: i64 = 1_000;
pub const REFRESH_SECONDS: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractSide {
    Yes,
    No,
}

impl ContractSide {
    const fn buy_book(self) -> (&'static str, &'static str) {
        match self {
            Self::Yes => ("yes_ask", "ASC"),
            Self::No => ("yes_bid", "DESC"),
        }
    }

    const fn sell_book(self) -> (&'static str, &'static str) {
        match self {
            Self::Yes => ("yes_bid", "DESC"),
            Self::No => ("yes_ask", "ASC"),
        }
    }

    const fn buy_action(self) -> &'static str {
        match self {
            Self::Yes => "buy_yes",
            Self::No => "buy_no",
        }
    }

    const fn sell_action(self) -> &'static str {
        match self {
            Self::Yes => "sell_yes",
            Self::No => "sell_no",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookSide {
    YesAsk,
    YesBid,
}

impl BookSide {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::YesAsk => "yes_ask",
            Self::YesBid => "yes_bid",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewLevel {
    pub side: BookSide,
    pub price: i64,
    pub size: i64,
}

impl NewLevel {
    #[must_use]
    pub const fn ask(price: i64, size: i64) -> Self {
        Self {
            side: BookSide::YesAsk,
            price,
            size,
        }
    }

    #[must_use]
    pub const fn bid(price: i64, size: i64) -> Self {
        Self {
            side: BookSide::YesBid,
            price,
            size,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    pub current_price: Option<i64>,
    pub yes_asks: Vec<(i64, i64)>,
    pub yes_bids: Vec<(i64, i64)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub prediction_id: i64,
    pub discord_id: i64,
    pub yes_contracts: i64,
    pub yes_cost_basis_total: i64,
    pub no_contracts: i64,
    pub no_cost_basis_total: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    pub prediction_id: i64,
    pub guild_id: i64,
    pub creator_id: i64,
    pub question: String,
    pub status: String,
    pub outcome: Option<String>,
    pub current_price: Option<i64>,
    pub initial_fair: Option<i64>,
    pub prev_price: Option<i64>,
    pub last_refresh_at: Option<i64>,
    pub lp_pnl: i64,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
    pub resolved_by: Option<i64>,
    pub channel_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub embed_message_id: Option<i64>,
    pub channel_message_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trade {
    pub trade_id: i64,
    pub discord_id: i64,
    pub action: String,
    pub contracts: i64,
    pub jopacoins: i64,
    pub vwap_x100: i64,
    pub last_fill_price: Option<i64>,
    pub trade_time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeQuote {
    pub contracts: i64,
    pub total: i64,
    pub fills: Vec<(i64, i64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeReceipt {
    pub guild_id: i64,
    pub trade_id: i64,
    pub trade_time: i64,
    pub side: ContractSide,
    pub contracts: i64,
    pub total: i64,
    pub vwap_x100: i64,
    pub fills: Vec<(i64, i64)>,
    pub new_balance: i64,
    pub yes_contracts: i64,
    pub no_contracts: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketSnapshot {
    pub market: Market,
    pub book: Book,
    pub recent_trades: Vec<Trade>,
    pub viewer_position: Option<Position>,
    pub volume_since_refresh: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenMarketSummary {
    pub market: Market,
    pub top_ask: Option<i64>,
    pub top_bid: Option<i64>,
    pub volume_recent: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkedPosition {
    pub position: Position,
    pub yes_mark: Option<i64>,
    pub no_mark: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPositionSummary {
    pub question: String,
    pub current_price: Option<i64>,
    pub status: String,
    pub marked: MarkedPosition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancellationRefund {
    pub discord_id: i64,
    pub refund: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancellation {
    pub prediction_id: i64,
    pub guild_id: i64,
    pub refunded: Vec<CancellationRefund>,
    pub total_refunded: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivePredictionEffects {
    pub payout_multiplier_bps: i64,
    pub spread_ticks_delta: i64,
}

impl Default for ActivePredictionEffects {
    fn default() -> Self {
        Self {
            payout_multiplier_bps: 10_000,
            spread_ticks_delta: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementParticipant {
    pub discord_id: i64,
    pub yes_contracts: i64,
    pub no_contracts: i64,
    pub winning_contracts: i64,
    pub cost_basis: i64,
    pub payout: i64,
    pub profit: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    pub prediction_id: i64,
    pub guild_id: i64,
    pub outcome: ContractSide,
    pub participants: Vec<SettlementParticipant>,
    pub total_payout: i64,
    pub lp_pnl: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserOrderbookStats {
    pub realized_pnl: i64,
    pub wins: i64,
    pub losses: i64,
    pub resolved_markets: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PnlHistoryPoint {
    pub prediction_id: i64,
    pub settle_time: i64,
    pub delta: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetWorthEntry {
    pub discord_id: i64,
    pub name: String,
    pub jopacoin_balance: i64,
    pub net_worth: i64,
}

#[derive(Debug, Error)]
pub enum PredictionRepositoryError {
    #[error("Prediction not found.")]
    PredictionNotFound,
    #[error("Market is not open for trading.")]
    MarketNotOpen,
    #[error("side must be 'yes' or 'no'")]
    InvalidSide,
    #[error("contracts must be positive")]
    InvalidContracts,
    #[error("contracts capped at {maximum} per trade.")]
    TradeTooLarge { maximum: i64 },
    #[error("Invalid status: {0}")]
    InvalidStatus(String),
    #[error(
        "Insufficient depth: only {available} contracts available (requested {requested}). Wait for next refresh."
    )]
    InsufficientDepth { available: i64, requested: i64 },
    #[error(
        "Insufficient depth on the bid side: only {available} contracts can be sold (requested {requested}). Wait for next refresh."
    )]
    InsufficientSellDepth { available: i64, requested: i64 },
    #[error("Player not found. Use /player register first.")]
    PlayerNotFound,
    #[error("You cannot trade contracts while in debt. Win some games first.")]
    InDebt,
    #[error("Insufficient balance: need {needed}, have {available}.")]
    InsufficientBalance { needed: i64, available: i64 },
    #[error("You only hold {held} contracts (requested to sell {requested}).")]
    InsufficientHoldings { held: i64, requested: i64 },
    #[error("Cannot settle market in status '{0}'.")]
    CannotSettle(String),
    #[error("Cannot cancel market in status '{0}'.")]
    CannotCancel(String),
    #[error("Invalid outcome")]
    InvalidOutcome,
    #[error("Invalid book level: price and size must be non-negative SQLite integers")]
    InvalidLevel,
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("prediction arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error("prediction SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Python's 10:1-split quote rounding. Ties favor the house.
pub fn quote_total(raw_jopa_x10: i64, buy: bool) -> i64 {
    if raw_jopa_x10 <= 0 {
        return 0;
    }
    if buy {
        ((raw_jopa_x10 + 5) / 10).max(1)
    } else {
        (raw_jopa_x10 + 4) / 10
    }
}

#[derive(Clone, Debug)]
pub struct PredictionRepository {
    path: PathBuf,
}

impl PredictionRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(value) => value,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Verify the order-book migration is present without changing schema.
    pub fn schema_contract(&self) -> Result<bool, PredictionRepositoryError> {
        let connection = self.connection()?;
        let tables = [
            "predictions",
            "prediction_levels",
            "prediction_positions",
            "prediction_trades",
            "prediction_fair_snapshots",
        ];
        for table in tables {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )?;
            if !exists {
                return Ok(false);
            }
        }
        let prediction_columns = table_columns(&connection, "predictions")?;
        let trade_columns = table_columns(&connection, "prediction_trades")?;
        Ok(
            ["current_price", "initial_fair", "last_refresh_at", "lp_pnl"]
                .into_iter()
                .all(|column| prediction_columns.contains_key(column))
                && trade_columns.contains_key("last_fill_price"),
        )
    }

    pub fn create_orderbook_prediction(
        &self,
        guild_id: Option<i64>,
        creator_id: i64,
        question: &str,
        initial_fair: i64,
        channel_id: Option<i64>,
        initial_levels: &[NewLevel],
    ) -> Result<i64, PredictionRepositoryError> {
        validate_levels(initial_levels)?;
        let now = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO predictions (
                 guild_id, creator_id, question, status, channel_id,
                 created_at, closes_at, current_price, initial_fair,
                 last_refresh_at, lp_pnl
             ) VALUES (?1, ?2, ?3, 'open', ?4, ?5, 0, ?6, ?6, ?5, 0)",
            params![
                Self::normalize_guild_id(guild_id),
                creator_id,
                question,
                channel_id,
                now,
                initial_fair
            ],
        )?;
        let prediction_id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO prediction_fair_snapshots
                 (market_id, guild_id, snapshot_at, fair_pct, reason)
             VALUES (?1, ?2, ?3, ?4, 'create')",
            params![
                prediction_id,
                Self::normalize_guild_id(guild_id),
                now,
                initial_fair
            ],
        )?;
        insert_levels(&transaction, prediction_id, initial_levels, now)?;
        transaction.commit()?;
        Ok(prediction_id)
    }

    pub fn update_discord_ids(
        &self,
        prediction_id: i64,
        thread_id: Option<i64>,
        embed_message_id: Option<i64>,
        channel_message_id: Option<i64>,
    ) -> Result<(), PredictionRepositoryError> {
        let connection = self.connection()?;
        if let Some(value) = thread_id {
            connection.execute(
                "UPDATE predictions SET thread_id=?1 WHERE prediction_id=?2",
                params![value, prediction_id],
            )?;
        }
        if let Some(value) = embed_message_id {
            connection.execute(
                "UPDATE predictions SET embed_message_id=?1 WHERE prediction_id=?2",
                params![value, prediction_id],
            )?;
        }
        if let Some(value) = channel_message_id {
            connection.execute(
                "UPDATE predictions SET channel_message_id=?1 WHERE prediction_id=?2",
                params![value, prediction_id],
            )?;
        }
        Ok(())
    }

    pub fn get_prediction(
        &self,
        prediction_id: i64,
    ) -> Result<Option<Market>, PredictionRepositoryError> {
        let connection = self.connection()?;
        select_market(&connection, prediction_id).map_err(Into::into)
    }

    /// Resolve a persistent market component from its durable Discord thread.
    ///
    /// Prediction buttons use fixed custom IDs, so the thread association is
    /// the restart-safe identity rather than process-local view state.
    pub fn get_prediction_by_thread(
        &self,
        thread_id: i64,
    ) -> Result<Option<Market>, PredictionRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT prediction_id,guild_id,creator_id,question,status,outcome,
                        current_price,initial_fair,prev_price,last_refresh_at,
                        COALESCE(lp_pnl,0),created_at,resolved_at,resolved_by,
                        channel_id,thread_id,embed_message_id,channel_message_id
                 FROM predictions WHERE thread_id=?1 ORDER BY prediction_id DESC LIMIT 1",
                [thread_id],
                market_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_markets_by_status(
        &self,
        guild_id: Option<i64>,
        status: &str,
    ) -> Result<Vec<Market>, PredictionRepositoryError> {
        if !matches!(status, "open" | "locked" | "resolved" | "cancelled") {
            return Err(PredictionRepositoryError::InvalidStatus(status.to_owned()));
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT prediction_id,guild_id,creator_id,question,status,outcome,
                    current_price,initial_fair,prev_price,last_refresh_at,
                    COALESCE(lp_pnl,0),created_at,resolved_at,resolved_by,
                    channel_id,thread_id,embed_message_id,channel_message_id
             FROM predictions WHERE guild_id=?1 AND status=?2
             ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map(
            params![Self::normalize_guild_id(guild_id), status],
            market_from_row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn player_wallet(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, PredictionRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Match `require_gamba_channel`: missing players are a silent no-op, and
    /// a debit updates the historical low-water mark without overdraft checks.
    pub fn debit_gamba_channel_penalty(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, PredictionRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)-1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )?;
        if changed == 1 {
            transaction.execute(
                "UPDATE players SET lowest_balance_ever=jopacoin_balance
                 WHERE discord_id=?1 AND guild_id=?2
                   AND (lowest_balance_ever IS NULL OR jopacoin_balance<lowest_balance_ever)",
                params![discord_id, guild_id],
            )?;
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn set_status(
        &self,
        prediction_id: i64,
        status: &str,
    ) -> Result<(), PredictionRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE predictions SET status=?1 WHERE prediction_id=?2",
            params![status, prediction_id],
        )?;
        Ok(())
    }

    pub fn replace_levels(
        &self,
        prediction_id: i64,
        levels: &[NewLevel],
    ) -> Result<(), PredictionRepositoryError> {
        validate_levels(levels)?;
        let now = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM prediction_levels WHERE prediction_id=?1",
            [prediction_id],
        )?;
        insert_levels(&transaction, prediction_id, levels, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn get_book(&self, prediction_id: i64) -> Result<Book, PredictionRepositoryError> {
        let connection = self.connection()?;
        read_book(&connection, prediction_id).map_err(Into::into)
    }

    pub fn get_position(
        &self,
        prediction_id: i64,
        discord_id: i64,
    ) -> Result<Option<Position>, PredictionRepositoryError> {
        let connection = self.connection()?;
        read_position(&connection, prediction_id, discord_id).map_err(Into::into)
    }

    pub fn quote_buy_contracts(
        &self,
        prediction_id: i64,
        side: ContractSide,
        contracts: i64,
    ) -> Result<TradeQuote, PredictionRepositoryError> {
        self.quote_buy_contracts_with_max(prediction_id, side, contracts, MAX_CONTRACTS_PER_TRADE)
    }

    pub fn quote_buy_contracts_with_max(
        &self,
        prediction_id: i64,
        side: ContractSide,
        contracts: i64,
        maximum: i64,
    ) -> Result<TradeQuote, PredictionRepositoryError> {
        validate_contracts(contracts, maximum)?;
        let connection = self.connection()?;
        ensure_open(&connection, prediction_id)?;
        let fills = plan_fills(
            &connection,
            prediction_id,
            side.buy_book(),
            contracts,
            false,
        )?;
        Ok(quote_from_fills(side, contracts, &fills, true))
    }

    pub fn buy_contracts_atomic(
        &self,
        prediction_id: i64,
        discord_id: i64,
        side: ContractSide,
        contracts: i64,
    ) -> Result<TradeReceipt, PredictionRepositoryError> {
        self.buy_contracts_atomic_with_max(
            prediction_id,
            discord_id,
            side,
            contracts,
            MAX_CONTRACTS_PER_TRADE,
        )
    }

    pub fn buy_contracts_atomic_with_max(
        &self,
        prediction_id: i64,
        discord_id: i64,
        side: ContractSide,
        contracts: i64,
        maximum: i64,
    ) -> Result<TradeReceipt, PredictionRepositoryError> {
        validate_contracts(contracts, maximum)?;
        let now = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guild_id = open_market_guild(&transaction, prediction_id)?;
        let fills = plan_fills(
            &transaction,
            prediction_id,
            side.buy_book(),
            contracts,
            false,
        )?;
        let quote = quote_from_fills(side, contracts, &fills, true);
        let balance = player_balance(&transaction, discord_id, guild_id)?;
        if balance < 0 {
            return Err(PredictionRepositoryError::InDebt);
        }
        if balance < quote.total {
            return Err(PredictionRepositoryError::InsufficientBalance {
                needed: quote.total,
                available: balance,
            });
        }
        set_ledger_context(
            &transaction,
            "prediction",
            prediction_id,
            "prediction contract purchase",
        )?;
        let debit_result = transaction.execute(
            "UPDATE players SET jopacoin_balance=jopacoin_balance-?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![quote.total, discord_id, guild_id],
        );
        clear_ledger_context(&transaction)?;
        if debit_result? != 1 {
            return Err(PredictionRepositoryError::PlayerNotFound);
        }
        consume_fills(&transaction, prediction_id, &fills)?;
        let mut position =
            read_position(&transaction, prediction_id, discord_id)?.unwrap_or(Position {
                prediction_id,
                discord_id,
                ..Position::default()
            });
        match side {
            ContractSide::Yes => {
                position.yes_contracts += contracts;
                position.yes_cost_basis_total += quote.total;
            }
            ContractSide::No => {
                position.no_contracts += contracts;
                position.no_cost_basis_total += quote.total;
            }
        }
        write_position(&transaction, position)?;
        let weighted = weighted_pct(side, &fills)?;
        let vwap_x100 = (weighted * 100 + contracts / 2) / contracts;
        let last_fill_price = fills.last().map_or(0, |fill| fill.price);
        transaction.execute(
            "INSERT INTO prediction_trades (
                 prediction_id, discord_id, action, contracts, jopacoins,
                 vwap_x100, last_fill_price, trade_time
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                prediction_id,
                discord_id,
                side.buy_action(),
                contracts,
                quote.total,
                vwap_x100,
                last_fill_price,
                now
            ],
        )?;
        let trade_id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE predictions SET lp_pnl=COALESCE(lp_pnl,0)+?1 WHERE prediction_id=?2",
            params![quote.total, prediction_id],
        )?;
        transaction.commit()?;
        Ok(TradeReceipt {
            guild_id,
            trade_id,
            trade_time: now,
            side,
            contracts,
            total: quote.total,
            vwap_x100,
            fills: quote.fills,
            new_balance: balance - quote.total,
            yes_contracts: position.yes_contracts,
            no_contracts: position.no_contracts,
        })
    }

    pub fn sell_contracts_atomic(
        &self,
        prediction_id: i64,
        discord_id: i64,
        side: ContractSide,
        contracts: i64,
    ) -> Result<TradeReceipt, PredictionRepositoryError> {
        validate_contracts(contracts, MAX_CONTRACTS_PER_TRADE)?;
        let now = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guild_id = open_market_guild(&transaction, prediction_id)?;
        let mut position =
            read_position(&transaction, prediction_id, discord_id)?.unwrap_or(Position {
                prediction_id,
                discord_id,
                ..Position::default()
            });
        let (old_qty, old_basis) = match side {
            ContractSide::Yes => (position.yes_contracts, position.yes_cost_basis_total),
            ContractSide::No => (position.no_contracts, position.no_cost_basis_total),
        };
        if old_qty < contracts {
            return Err(PredictionRepositoryError::InsufficientHoldings {
                held: old_qty,
                requested: contracts,
            });
        }
        let fills = plan_fills(
            &transaction,
            prediction_id,
            side.sell_book(),
            contracts,
            true,
        )?;
        let quote = quote_from_fills(side, contracts, &fills, false);
        // Match Python's failure order: a missing player aborts before book or
        // position mutations and rolls the complete transaction back.
        player_balance(&transaction, discord_id, guild_id)?;
        set_ledger_context(
            &transaction,
            "prediction",
            prediction_id,
            "prediction contract sale",
        )?;
        let credit_result = transaction.execute(
            "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![quote.total, discord_id, guild_id],
        );
        clear_ledger_context(&transaction)?;
        if credit_result? != 1 {
            return Err(PredictionRepositoryError::PlayerNotFound);
        }
        consume_fills(&transaction, prediction_id, &fills)?;
        let basis_reduction = old_basis * contracts / old_qty;
        match side {
            ContractSide::Yes => {
                position.yes_contracts -= contracts;
                position.yes_cost_basis_total -= basis_reduction;
            }
            ContractSide::No => {
                position.no_contracts -= contracts;
                position.no_cost_basis_total -= basis_reduction;
            }
        }
        write_position(&transaction, position)?;
        let weighted = weighted_pct(side, &fills)?;
        let vwap_x100 = (weighted * 100 + contracts / 2) / contracts;
        let last_fill_price = fills.last().map_or(0, |fill| fill.price);
        transaction.execute(
            "INSERT INTO prediction_trades (
                 prediction_id, discord_id, action, contracts, jopacoins,
                 vwap_x100, last_fill_price, trade_time
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                prediction_id,
                discord_id,
                side.sell_action(),
                contracts,
                -quote.total,
                vwap_x100,
                last_fill_price,
                now
            ],
        )?;
        let trade_id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE predictions SET lp_pnl=COALESCE(lp_pnl,0)-?1 WHERE prediction_id=?2",
            params![quote.total, prediction_id],
        )?;
        let new_balance = player_balance(&transaction, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(TradeReceipt {
            guild_id,
            trade_id,
            trade_time: now,
            side,
            contracts,
            total: quote.total,
            vwap_x100,
            fills: quote.fills,
            new_balance,
            yes_contracts: position.yes_contracts,
            no_contracts: position.no_contracts,
        })
    }

    pub fn transfer_position_contracts(
        &self,
        prediction_id: i64,
        from_discord_id: i64,
        to_discord_id: i64,
        side: ContractSide,
        requested: i64,
    ) -> Result<Option<i64>, PredictionRepositoryError> {
        if requested <= 0 {
            return Err(PredictionRepositoryError::InvalidContracts);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((status, guild_id)) = transaction
            .query_row(
                "SELECT status,guild_id FROM predictions WHERE prediction_id=?1",
                [prediction_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if status != "open" {
            return Ok(None);
        }
        let registered: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
            params![to_discord_id, guild_id],
            |row| row.get(0),
        )?;
        if !registered {
            return Ok(None);
        }
        let Some(mut source) = read_position(&transaction, prediction_id, from_discord_id)? else {
            return Ok(None);
        };
        let (held, basis) = match side {
            ContractSide::Yes => (source.yes_contracts, source.yes_cost_basis_total),
            ContractSide::No => (source.no_contracts, source.no_cost_basis_total),
        };
        if held <= 0 {
            return Ok(None);
        }
        let moved = requested.min(held);
        let moved_basis = if moved == held {
            basis
        } else {
            basis * moved / held
        };
        let mut target =
            read_position(&transaction, prediction_id, to_discord_id)?.unwrap_or(Position {
                prediction_id,
                discord_id: to_discord_id,
                ..Position::default()
            });
        match side {
            ContractSide::Yes => {
                source.yes_contracts -= moved;
                source.yes_cost_basis_total -= moved_basis;
                target.yes_contracts += moved;
                target.yes_cost_basis_total += moved_basis;
            }
            ContractSide::No => {
                source.no_contracts -= moved;
                source.no_cost_basis_total -= moved_basis;
                target.no_contracts += moved;
                target.no_cost_basis_total += moved_basis;
            }
        }
        write_position(&transaction, source)?;
        write_position(&transaction, target)?;
        transaction.commit()?;
        Ok(Some(moved))
    }

    pub fn get_market_snapshot(
        &self,
        prediction_id: i64,
        viewer_id: Option<i64>,
        recent_limit: i64,
    ) -> Result<Option<MarketSnapshot>, PredictionRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let Some(market) = select_market(&transaction, prediction_id)? else {
            return Ok(None);
        };
        let book = read_book(&transaction, prediction_id)?;
        let recent_trades = read_recent_trades(&transaction, prediction_id, recent_limit)?;
        let viewer_position = match viewer_id {
            Some(discord_id) => read_position(&transaction, prediction_id, discord_id)?,
            None => None,
        };
        let volume_since_refresh = transaction.query_row(
            "SELECT COALESCE(SUM(contracts),0) FROM prediction_trades
             WHERE prediction_id=?1 AND trade_time>=?2",
            params![prediction_id, market.last_refresh_at.unwrap_or(0)],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Some(MarketSnapshot {
            market,
            book,
            recent_trades,
            viewer_position,
            volume_since_refresh,
        }))
    }

    pub fn list_open_markets(
        &self,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Vec<OpenMarketSummary>, PredictionRepositoryError> {
        self.list_open_markets_with_refresh(guild_id, now, REFRESH_SECONDS)
    }

    pub fn list_open_markets_with_refresh(
        &self,
        guild_id: Option<i64>,
        now: i64,
        refresh_seconds: i64,
    ) -> Result<Vec<OpenMarketSummary>, PredictionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT p.prediction_id,p.guild_id,p.creator_id,p.question,p.status,p.outcome,
                    p.current_price,p.initial_fair,p.prev_price,p.last_refresh_at,
                    COALESCE(p.lp_pnl,0),p.created_at,p.resolved_at,p.resolved_by,
                    p.channel_id,p.thread_id,p.embed_message_id,p.channel_message_id,
                    (SELECT MIN(price) FROM prediction_levels
                     WHERE prediction_id=p.prediction_id AND side='yes_ask' AND remaining_size>0),
                    (SELECT MAX(price) FROM prediction_levels
                     WHERE prediction_id=p.prediction_id AND side='yes_bid' AND remaining_size>0),
                    CAST(COALESCE((SELECT SUM(contracts) FROM prediction_trades
                     WHERE prediction_id=p.prediction_id AND trade_time>=?1),0) AS INTEGER)
             FROM predictions p WHERE p.guild_id=?2 AND p.status='open'
             ORDER BY p.created_at DESC",
        )?;
        let rows = statement.query_map(
            params![
                now.saturating_sub(refresh_seconds.max(0)),
                Self::normalize_guild_id(guild_id)
            ],
            |row| {
                Ok(OpenMarketSummary {
                    market: market_from_row(row)?,
                    top_ask: row.get(18)?,
                    top_bid: row.get(19)?,
                    volume_recent: row.get(20)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_user_open_positions(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<MarkedPosition>, PredictionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT pp.prediction_id,pp.discord_id,pp.yes_contracts,
                    pp.yes_cost_basis_total,pp.no_contracts,pp.no_cost_basis_total,
                    (SELECT MAX(price) FROM prediction_levels
                     WHERE prediction_id=pp.prediction_id AND side='yes_bid' AND remaining_size>0),
                    (SELECT 100-MIN(price) FROM prediction_levels
                     WHERE prediction_id=pp.prediction_id AND side='yes_ask' AND remaining_size>0)
             FROM prediction_positions pp JOIN predictions p USING(prediction_id)
             WHERE pp.discord_id=?1 AND p.guild_id=?2 AND p.status='open'
               AND (pp.yes_contracts>0 OR pp.no_contracts>0)
             ORDER BY p.created_at DESC",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| {
                Ok(MarkedPosition {
                    position: Position {
                        prediction_id: row.get(0)?,
                        discord_id: row.get(1)?,
                        yes_contracts: row.get(2)?,
                        yes_cost_basis_total: row.get(3)?,
                        no_contracts: row.get(4)?,
                        no_cost_basis_total: row.get(5)?,
                    },
                    yes_mark: row.get(6)?,
                    no_mark: row.get(7)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_user_open_position_summaries(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<OpenPositionSummary>, PredictionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT p.question,p.current_price,p.status,
                    pp.prediction_id,pp.discord_id,pp.yes_contracts,
                    pp.yes_cost_basis_total,pp.no_contracts,pp.no_cost_basis_total,
                    (SELECT MAX(price) FROM prediction_levels
                     WHERE prediction_id=pp.prediction_id AND side='yes_bid'
                       AND remaining_size>0),
                    (SELECT 100-MIN(price) FROM prediction_levels
                     WHERE prediction_id=pp.prediction_id AND side='yes_ask'
                       AND remaining_size>0)
             FROM prediction_positions pp JOIN predictions p USING(prediction_id)
             WHERE pp.discord_id=?1 AND p.guild_id=?2 AND p.status='open'
               AND (pp.yes_contracts>0 OR pp.no_contracts>0)
             ORDER BY p.created_at DESC",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| {
                Ok(OpenPositionSummary {
                    question: row.get(0)?,
                    current_price: row.get(1)?,
                    status: row.get(2)?,
                    marked: MarkedPosition {
                        position: Position {
                            prediction_id: row.get(3)?,
                            discord_id: row.get(4)?,
                            yes_contracts: row.get(5)?,
                            yes_cost_basis_total: row.get(6)?,
                            no_contracts: row.get(7)?,
                            no_cost_basis_total: row.get(8)?,
                        },
                        yes_mark: row.get(9)?,
                        no_mark: row.get(10)?,
                    },
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn active_prediction_effects(
        &self,
        guild_id: Option<i64>,
        now: i64,
        enabled: bool,
    ) -> Result<ActivePredictionEffects, PredictionRepositoryError> {
        if !enabled {
            return Ok(ActivePredictionEffects::default());
        }
        let connection = self.connection()?;
        let effects: Option<(f64, i64)> = connection
            .query_row(
                "SELECT
                     CAST(COALESCE(CASE WHEN json_valid(effects)
                         THEN json_extract(effects,'$.prediction_payout_multiplier') END,1.0)
                         AS REAL),
                     CAST(COALESCE(CASE WHEN json_valid(effects)
                         THEN json_extract(effects,'$.prediction_spread_ticks_delta') END,0)
                         AS INTEGER)
                 FROM economy_daily_events
                 WHERE guild_id=?1 AND starts_at<=?2 AND ?2<ends_at
                 ORDER BY starts_at DESC LIMIT 1",
                params![Self::normalize_guild_id(guild_id), now],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((multiplier, spread_ticks_delta)) = effects else {
            return Ok(ActivePredictionEffects::default());
        };
        let multiplier = if multiplier.is_finite() {
            multiplier.max(0.0)
        } else {
            1.0
        };
        Ok(ActivePredictionEffects {
            payout_multiplier_bps: python_round_i64(multiplier * 10_000.0),
            spread_ticks_delta,
        })
    }

    pub fn apply_refresh(
        &self,
        prediction_id: i64,
        new_price: i64,
        levels: &[NewLevel],
        now: i64,
        reason: &str,
        min_quote_offset: i64,
    ) -> Result<bool, PredictionRepositoryError> {
        validate_levels(levels)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((status, guild_id)) = transaction
            .query_row(
                "SELECT status,guild_id FROM predictions WHERE prediction_id=?1",
                [prediction_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(false);
        };
        if status != "open" {
            return Ok(false);
        }
        if min_quote_offset > 0 {
            transaction.execute(
                "DELETE FROM prediction_levels WHERE prediction_id=?1 AND (
                    (side='yes_ask' AND price>?2 AND price<?3) OR
                    (side='yes_bid' AND price<?2 AND price>?4))",
                params![
                    prediction_id,
                    new_price,
                    new_price + min_quote_offset,
                    new_price - min_quote_offset
                ],
            )?;
        }
        for level in levels {
            let level_id: Option<i64> = transaction
                .query_row(
                    "SELECT level_id FROM prediction_levels
                     WHERE prediction_id=?1 AND side=?2 AND price=?3",
                    params![prediction_id, level.side.as_str(), level.price],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(level_id) = level_id {
                transaction.execute(
                    "UPDATE prediction_levels
                     SET remaining_size=remaining_size+?1,posted_at=?2 WHERE level_id=?3",
                    params![level.size, now, level_id],
                )?;
            } else {
                insert_level(&transaction, prediction_id, *level, now)?;
            }
        }
        transaction.execute(
            "UPDATE predictions SET prev_price=current_price,current_price=?1,
                 last_refresh_at=?2 WHERE prediction_id=?3",
            params![new_price, now, prediction_id],
        )?;
        transaction.execute(
            "INSERT INTO prediction_fair_snapshots
                 (market_id,guild_id,snapshot_at,fair_pct,reason)
             VALUES (?1,?2,?3,?4,?5)",
            params![prediction_id, guild_id, now, new_price, reason],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn markets_due_for_refresh(
        &self,
        now: i64,
    ) -> Result<Vec<Market>, PredictionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT prediction_id,guild_id,creator_id,question,status,outcome,
                    current_price,initial_fair,prev_price,last_refresh_at,
                    COALESCE(lp_pnl,0),created_at,resolved_at,resolved_by,
                    channel_id,thread_id,embed_message_id,channel_message_id
             FROM predictions WHERE status='open'
               AND COALESCE(last_refresh_at,0)<=?1
             ORDER BY COALESCE(last_refresh_at,0) ASC",
        )?;
        let rows = statement.query_map([now - REFRESH_SECONDS], market_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn recent_trades(
        &self,
        prediction_id: i64,
        limit: i64,
    ) -> Result<Vec<Trade>, PredictionRepositoryError> {
        let connection = self.connection()?;
        read_recent_trades(&connection, prediction_id, limit).map_err(Into::into)
    }

    pub fn trade_count_since(
        &self,
        prediction_id: i64,
        since: i64,
    ) -> Result<i64, PredictionRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM prediction_trades
             WHERE prediction_id=?1 AND trade_time>=?2",
            params![prediction_id, since],
            |row| row.get(0),
        )?)
    }

    pub fn last_fill_price_since(
        &self,
        prediction_id: i64,
        actions: &[&str],
        since: i64,
    ) -> Result<Option<i64>, PredictionRepositoryError> {
        if actions.is_empty() {
            return Ok(None);
        }
        let connection = self.connection()?;
        // The service only asks the two fixed action pairs. Keeping the SQL
        // static avoids interpolating caller input into a shared-DB query.
        let pair = match actions {
            ["buy_yes", "sell_no"] => ("buy_yes", "sell_no"),
            ["buy_no", "sell_yes"] => ("buy_no", "sell_yes"),
            _ => return Ok(None),
        };
        Ok(connection
            .query_row(
                "SELECT last_fill_price FROM prediction_trades
                 WHERE prediction_id=?1 AND trade_time>=?2
                   AND action IN (?3,?4) AND last_fill_price IS NOT NULL
                 ORDER BY trade_id DESC LIMIT 1",
                params![prediction_id, since, pair.0, pair.1],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn fair_history(
        &self,
        prediction_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<(i64, i64)>, PredictionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT snapshot_at,fair_pct FROM prediction_fair_snapshots
             WHERE market_id=?1 AND guild_id=?2 ORDER BY snapshot_at ASC",
        )?;
        let rows = statement.query_map(
            params![prediction_id, Self::normalize_guild_id(guild_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn pop_one_shot_flag(
        &self,
        guild_id: Option<i64>,
        key: &str,
    ) -> Result<bool, PredictionRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let value: Option<String> = transaction
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
                params![guild_id, key],
                |row| row.get(0),
            )
            .optional()?;
        if value.as_deref() != Some("0") {
            return Ok(false);
        }
        transaction.execute(
            "UPDATE app_kv SET value='1' WHERE guild_id=?1 AND key=?2",
            params![guild_id, key],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// WARNING — no production caller. This settlement applies no payout
    /// multiplier, bankruptcy penalty, or vanity tax; the live settlement is
    /// `prediction_resolution_repository::settle_orderbook_at`. Do not wire
    /// this variant to the runtime.
    pub fn settle_orderbook(
        &self,
        prediction_id: i64,
        outcome: ContractSide,
        resolved_by: Option<i64>,
    ) -> Result<Settlement, PredictionRepositoryError> {
        let now = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, guild_id, lp_pnl): (String, i64, i64) = transaction
            .query_row(
                "SELECT status,guild_id,COALESCE(lp_pnl,0) FROM predictions
                 WHERE prediction_id=?1",
                [prediction_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(PredictionRepositoryError::PredictionNotFound)?;
        if status != "open" && status != "locked" {
            return Err(PredictionRepositoryError::CannotSettle(status));
        }
        let mut statement = transaction.prepare(
            "SELECT prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                    no_contracts,no_cost_basis_total
             FROM prediction_positions WHERE prediction_id=?1 ORDER BY discord_id",
        )?;
        let positions = statement
            .query_map([prediction_id], position_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut participants = Vec::with_capacity(positions.len());
        let mut total_payout = 0_i64;
        for position in positions {
            let winning_contracts = match outcome {
                ContractSide::Yes => position.yes_contracts,
                ContractSide::No => position.no_contracts,
            };
            let payout = winning_contracts
                .checked_mul(CONTRACT_VALUE)
                .ok_or(PredictionRepositoryError::ArithmeticOverflow)?;
            if payout > 0 {
                set_ledger_context(
                    &transaction,
                    "prediction_resolution",
                    prediction_id,
                    "prediction resolution payout",
                )?;
                let result = transaction.execute(
                    "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                         updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![payout, position.discord_id, guild_id],
                );
                clear_ledger_context(&transaction)?;
                if result? != 1 {
                    return Err(PredictionRepositoryError::PlayerNotFound);
                }
            }
            total_payout = total_payout
                .checked_add(payout)
                .ok_or(PredictionRepositoryError::ArithmeticOverflow)?;
            let cost_basis = position.yes_cost_basis_total + position.no_cost_basis_total;
            participants.push(SettlementParticipant {
                discord_id: position.discord_id,
                yes_contracts: position.yes_contracts,
                no_contracts: position.no_contracts,
                winning_contracts,
                cost_basis,
                payout,
                profit: payout - cost_basis,
            });
        }
        transaction.execute(
            "DELETE FROM prediction_levels WHERE prediction_id=?1",
            [prediction_id],
        )?;
        transaction.execute(
            "UPDATE predictions SET status='resolved',outcome=?1,resolved_at=?2,
                 resolved_by=?3,lp_pnl=COALESCE(lp_pnl,0)-?4 WHERE prediction_id=?5",
            params![
                match outcome {
                    ContractSide::Yes => "yes",
                    ContractSide::No => "no",
                },
                now,
                resolved_by,
                total_payout,
                prediction_id
            ],
        )?;
        transaction.commit()?;
        Ok(Settlement {
            prediction_id,
            guild_id,
            outcome,
            participants,
            total_payout,
            lp_pnl: lp_pnl - total_payout,
        })
    }

    pub fn cancel_orderbook(&self, prediction_id: i64) -> Result<i64, PredictionRepositoryError> {
        self.cancel_orderbook_detailed(prediction_id)
            .map(|result| result.total_refunded)
    }

    pub fn cancel_orderbook_detailed(
        &self,
        prediction_id: i64,
    ) -> Result<Cancellation, PredictionRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, guild_id): (String, i64) = transaction
            .query_row(
                "SELECT status,guild_id FROM predictions WHERE prediction_id=?1",
                [prediction_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(PredictionRepositoryError::PredictionNotFound)?;
        if status != "open" {
            return Err(PredictionRepositoryError::CannotCancel(status));
        }
        let mut statement = transaction.prepare(
            "SELECT discord_id,yes_cost_basis_total+no_cost_basis_total
             FROM prediction_positions WHERE prediction_id=?1",
        )?;
        let holders = statement
            .query_map([prediction_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut total = 0_i64;
        let mut refunded = Vec::with_capacity(holders.len());
        for (discord_id, refund) in holders {
            if refund > 0 {
                set_ledger_context(
                    &transaction,
                    "prediction_refund",
                    prediction_id,
                    "cancelled prediction cost-basis refund",
                )?;
                let result = transaction.execute(
                    "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                         updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![refund, discord_id, guild_id],
                );
                clear_ledger_context(&transaction)?;
                if result? != 1 {
                    return Err(PredictionRepositoryError::PlayerNotFound);
                }
                total = total
                    .checked_add(refund)
                    .ok_or(PredictionRepositoryError::ArithmeticOverflow)?;
            }
            refunded.push(CancellationRefund { discord_id, refund });
        }
        transaction.execute(
            "DELETE FROM prediction_levels WHERE prediction_id=?1",
            [prediction_id],
        )?;
        transaction.execute(
            "DELETE FROM prediction_positions WHERE prediction_id=?1",
            [prediction_id],
        )?;
        transaction.execute(
            "UPDATE predictions SET status='cancelled' WHERE prediction_id=?1",
            [prediction_id],
        )?;
        transaction.commit()?;
        Ok(Cancellation {
            prediction_id,
            guild_id,
            refunded,
            total_refunded: total,
        })
    }

    pub fn user_orderbook_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<UserOrderbookStats, PredictionRepositoryError> {
        let history = self.player_pnl_history(discord_id, guild_id)?;
        let mut result = UserOrderbookStats {
            resolved_markets: i64::try_from(history.len())
                .map_err(|_| PredictionRepositoryError::ArithmeticOverflow)?,
            ..UserOrderbookStats::default()
        };
        for point in history {
            result.realized_pnl += point.delta;
            if point.delta > 0 {
                result.wins += 1;
            } else if point.delta < 0 {
                result.losses += 1;
            }
        }
        Ok(result)
    }

    /// Python parity (`prediction_repository.py::get_player_pnl_history`):
    /// realized P&L is the settlement ledger's actual net credit (after
    /// bankruptcy penalty and vanity tax) whenever ledger rows exist; the
    /// face-value calculation is only the legacy fallback for settlements
    /// predating the central ledger.
    pub fn player_pnl_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<PnlHistoryPoint>, PredictionRepositoryError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare(crate::prediction_resolution_repository::PNL_HISTORY_SQL)?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| {
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
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn net_worth_leaderboard(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<NetWorthEntry>, PredictionRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "WITH position_values AS (
                 SELECT pp.discord_id,SUM(
                     CAST(pp.yes_contracts*?1*COALESCE(p.current_price,p.initial_fair,50)/100 AS INTEGER)
                     +CAST(pp.no_contracts*?1*(100-COALESCE(p.current_price,p.initial_fair,50))/100 AS INTEGER)
                 ) AS value
                 FROM predictions p JOIN prediction_positions pp USING(prediction_id)
                 WHERE p.guild_id=?2 AND p.status IN ('open','locked')
                 GROUP BY pp.discord_id)
             SELECT players.discord_id,players.discord_username,
                    COALESCE(players.jopacoin_balance,0),
                    COALESCE(players.jopacoin_balance,0)+COALESCE(position_values.value,0)
             FROM players LEFT JOIN position_values USING(discord_id)
             WHERE players.guild_id=?2
             ORDER BY 4 DESC,3 DESC,players.discord_id ASC",
        )?;
        let rows = statement.query_map(
            params![CONTRACT_VALUE, Self::normalize_guild_id(guild_id)],
            |row| {
                Ok(NetWorthEntry {
                    discord_id: row.get(0)?,
                    name: row.get(1)?,
                    jopacoin_balance: row.get(2)?,
                    net_worth: row.get(3)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[derive(Clone, Copy)]
struct Fill {
    level_id: i64,
    price: i64,
    size: i64,
}

fn validate_contracts(contracts: i64, maximum: i64) -> Result<(), PredictionRepositoryError> {
    if contracts <= 0 {
        Err(PredictionRepositoryError::InvalidContracts)
    } else if contracts > maximum.max(0) {
        Err(PredictionRepositoryError::TradeTooLarge {
            maximum: maximum.max(0),
        })
    } else {
        Ok(())
    }
}

fn python_round_i64(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if (fraction - 0.5).abs() < f64::EPSILON {
        let floor = floor as i64;
        if floor % 2 == 0 { floor } else { floor + 1 }
    } else {
        value.round() as i64
    }
}

fn validate_levels(levels: &[NewLevel]) -> Result<(), PredictionRepositoryError> {
    if levels
        .iter()
        .any(|level| !(0..=99).contains(&level.price) || level.size < 0)
    {
        Err(PredictionRepositoryError::InvalidLevel)
    } else {
        Ok(())
    }
}

fn insert_levels(
    transaction: &Transaction<'_>,
    prediction_id: i64,
    levels: &[NewLevel],
    now: i64,
) -> Result<(), rusqlite::Error> {
    for level in levels {
        insert_level(transaction, prediction_id, *level, now)?;
    }
    Ok(())
}

fn insert_level(
    transaction: &Transaction<'_>,
    prediction_id: i64,
    level: NewLevel,
    now: i64,
) -> Result<(), rusqlite::Error> {
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
    Ok(())
}

fn ensure_open(
    connection: &Connection,
    prediction_id: i64,
) -> Result<(), PredictionRepositoryError> {
    open_market_guild(connection, prediction_id).map(|_| ())
}

fn open_market_guild(
    connection: &Connection,
    prediction_id: i64,
) -> Result<i64, PredictionRepositoryError> {
    let row: Option<(String, i64)> = connection
        .query_row(
            "SELECT status,guild_id FROM predictions WHERE prediction_id=?1",
            [prediction_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((status, guild_id)) = row else {
        return Err(PredictionRepositoryError::PredictionNotFound);
    };
    if status != "open" {
        return Err(PredictionRepositoryError::MarketNotOpen);
    }
    Ok(guild_id)
}

fn plan_fills(
    connection: &Connection,
    prediction_id: i64,
    book: (&str, &str),
    contracts: i64,
    selling: bool,
) -> Result<Vec<Fill>, PredictionRepositoryError> {
    let sql = if book.1 == "ASC" {
        "SELECT level_id,price,remaining_size FROM prediction_levels
         WHERE prediction_id=?1 AND side=?2 AND remaining_size>0 ORDER BY price ASC"
    } else {
        "SELECT level_id,price,remaining_size FROM prediction_levels
         WHERE prediction_id=?1 AND side=?2 AND remaining_size>0 ORDER BY price DESC"
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params![prediction_id, book.0], |row| {
        Ok(Fill {
            level_id: row.get(0)?,
            price: row.get(1)?,
            size: row.get(2)?,
        })
    })?;
    let mut remaining = contracts;
    let mut fills = Vec::new();
    for row in rows {
        if remaining == 0 {
            break;
        }
        let row = row?;
        let size = remaining.min(row.size);
        fills.push(Fill { size, ..row });
        remaining -= size;
    }
    if remaining > 0 {
        let available = contracts - remaining;
        return Err(if selling {
            PredictionRepositoryError::InsufficientSellDepth {
                available,
                requested: contracts,
            }
        } else {
            PredictionRepositoryError::InsufficientDepth {
                available,
                requested: contracts,
            }
        });
    }
    Ok(fills)
}

fn weighted_pct(side: ContractSide, fills: &[Fill]) -> Result<i64, PredictionRepositoryError> {
    fills.iter().try_fold(0_i64, |total, fill| {
        let price = match side {
            ContractSide::Yes => fill.price,
            ContractSide::No => 100 - fill.price,
        };
        total
            .checked_add(
                price
                    .checked_mul(fill.size)
                    .ok_or(PredictionRepositoryError::ArithmeticOverflow)?,
            )
            .ok_or(PredictionRepositoryError::ArithmeticOverflow)
    })
}

fn quote_from_fills(side: ContractSide, contracts: i64, fills: &[Fill], buy: bool) -> TradeQuote {
    let weighted = weighted_pct(side, fills).expect("validated market arithmetic");
    TradeQuote {
        contracts,
        total: quote_total(weighted, buy),
        fills: fills.iter().map(|fill| (fill.price, fill.size)).collect(),
    }
}

fn consume_fills(
    transaction: &Transaction<'_>,
    prediction_id: i64,
    fills: &[Fill],
) -> Result<(), rusqlite::Error> {
    for fill in fills {
        transaction.execute(
            "UPDATE prediction_levels SET remaining_size=remaining_size-?1 WHERE level_id=?2",
            params![fill.size, fill.level_id],
        )?;
    }
    transaction.execute(
        "DELETE FROM prediction_levels WHERE prediction_id=?1 AND remaining_size<=0",
        [prediction_id],
    )?;
    Ok(())
}

fn player_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<i64, PredictionRepositoryError> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(PredictionRepositoryError::PlayerNotFound)
}

fn read_position(
    connection: &Connection,
    prediction_id: i64,
    discord_id: i64,
) -> Result<Option<Position>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                    no_contracts,no_cost_basis_total
             FROM prediction_positions WHERE prediction_id=?1 AND discord_id=?2",
            params![prediction_id, discord_id],
            position_from_row,
        )
        .optional()
        .map(|position| {
            position.filter(|position| position.yes_contracts != 0 || position.no_contracts != 0)
        })
}

fn position_from_row(row: &Row<'_>) -> Result<Position, rusqlite::Error> {
    Ok(Position {
        prediction_id: row.get(0)?,
        discord_id: row.get(1)?,
        yes_contracts: row.get(2)?,
        yes_cost_basis_total: row.get(3)?,
        no_contracts: row.get(4)?,
        no_cost_basis_total: row.get(5)?,
    })
}

fn write_position(
    transaction: &Transaction<'_>,
    position: Position,
) -> Result<(), rusqlite::Error> {
    if position.yes_contracts == 0 && position.no_contracts == 0 {
        transaction.execute(
            "DELETE FROM prediction_positions WHERE prediction_id=?1 AND discord_id=?2",
            params![position.prediction_id, position.discord_id],
        )?;
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO prediction_positions (
             prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
             no_contracts,no_cost_basis_total
         ) VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(prediction_id,discord_id) DO UPDATE SET
             yes_contracts=excluded.yes_contracts,
             yes_cost_basis_total=excluded.yes_cost_basis_total,
             no_contracts=excluded.no_contracts,
             no_cost_basis_total=excluded.no_cost_basis_total",
        params![
            position.prediction_id,
            position.discord_id,
            position.yes_contracts,
            position.yes_cost_basis_total,
            position.no_contracts,
            position.no_cost_basis_total
        ],
    )?;
    Ok(())
}

fn read_book(connection: &Connection, prediction_id: i64) -> Result<Book, rusqlite::Error> {
    let current_price = connection
        .query_row(
            "SELECT current_price FROM predictions WHERE prediction_id=?1",
            [prediction_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let mut statement = connection.prepare(
        "SELECT side,price,remaining_size FROM prediction_levels
         WHERE prediction_id=?1 AND remaining_size>0",
    )?;
    let mut asks = Vec::new();
    let mut bids = Vec::new();
    let rows = statement.query_map([prediction_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (side, price, size) = row?;
        if side == "yes_ask" {
            asks.push((price, size));
        } else if side == "yes_bid" {
            bids.push((price, size));
        }
    }
    asks.sort_by_key(|level| level.0);
    bids.sort_by_key(|level| std::cmp::Reverse(level.0));
    Ok(Book {
        current_price,
        yes_asks: asks,
        yes_bids: bids,
    })
}

fn read_recent_trades(
    connection: &Connection,
    prediction_id: i64,
    limit: i64,
) -> Result<Vec<Trade>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT trade_id,discord_id,action,contracts,jopacoins,vwap_x100,
                last_fill_price,trade_time
         FROM prediction_trades WHERE prediction_id=?1
         ORDER BY trade_id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![prediction_id, limit], |row| {
        Ok(Trade {
            trade_id: row.get(0)?,
            discord_id: row.get(1)?,
            action: row.get(2)?,
            contracts: row.get(3)?,
            jopacoins: row.get(4)?,
            vwap_x100: row.get(5)?,
            last_fill_price: row.get(6)?,
            trade_time: row.get(7)?,
        })
    })?;
    rows.collect()
}

fn select_market(
    connection: &Connection,
    prediction_id: i64,
) -> Result<Option<Market>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT prediction_id,guild_id,creator_id,question,status,outcome,
                    current_price,initial_fair,prev_price,last_refresh_at,
                    COALESCE(lp_pnl,0),created_at,resolved_at,resolved_by,
                    channel_id,thread_id,embed_message_id,channel_message_id
             FROM predictions WHERE prediction_id=?1",
            [prediction_id],
            market_from_row,
        )
        .optional()
}

fn market_from_row(row: &Row<'_>) -> Result<Market, rusqlite::Error> {
    Ok(Market {
        prediction_id: row.get(0)?,
        guild_id: row.get(1)?,
        creator_id: row.get(2)?,
        question: row.get(3)?,
        status: row.get(4)?,
        outcome: row.get(5)?,
        current_price: row.get(6)?,
        initial_fair: row.get(7)?,
        prev_price: row.get(8)?,
        last_refresh_at: row.get(9)?,
        lp_pnl: row.get(10)?,
        created_at: row.get(11)?,
        resolved_at: row.get(12)?,
        resolved_by: row.get(13)?,
        channel_id: row.get(14)?,
        thread_id: row.get(15)?,
        embed_message_id: row.get(16)?,
        channel_message_id: row.get(17)?,
    })
}

fn table_columns(
    connection: &Connection,
    table: &str,
) -> Result<BTreeMap<String, i64>, rusqlite::Error> {
    let sql = match table {
        "predictions" => "PRAGMA table_info(predictions)",
        "prediction_trades" => "PRAGMA table_info(prediction_trades)",
        _ => return Ok(BTreeMap::new()),
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(1)?, row.get(0)?)))?;
    rows.collect()
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    prediction_id: i64,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id,source,related_type,related_id,reason)
         VALUES (1,?1,'prediction',?2,?3)
         ON CONFLICT(id) DO UPDATE SET source=excluded.source,
             related_type=excluded.related_type,related_id=excluded.related_id,
             reason=excluded.reason",
        params![source, prediction_id.to_string(), reason],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context WHERE id=1", [])?;
    Ok(())
}

fn unix_timestamp() -> Result<i64, PredictionRepositoryError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PredictionRepositoryError::ClockBeforeUnixEpoch)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| PredictionRepositoryError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;

    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::{
        BookSide, CONTRACT_VALUE, ContractSide, NewLevel, Position, PredictionRepository,
        PredictionRepositoryError, quote_total,
    };

    const GUILD: i64 = 101;
    const OTHER_GUILD: i64 = 202;

    static FIXTURE_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

    struct Fixture {
        file: NamedTempFile,
        repository: PredictionRepository,
    }

    impl Fixture {
        fn new() -> Self {
            let template = FIXTURE_DATABASE_TEMPLATE.get_or_init(|| {
                let file = NamedTempFile::new().expect("create prediction SQLite template");
                let connection = Connection::open(file.path()).expect("open prediction template");
                connection
                    .execute_batch(
                        "PRAGMA journal_mode=WAL;
                     CREATE TABLE players (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL DEFAULT 0,
                         discord_username TEXT NOT NULL,
                         jopacoin_balance INTEGER DEFAULT 3,
                         lowest_balance_ever INTEGER,
                         wins INTEGER DEFAULT 0,
                         glicko_rating REAL DEFAULT 0,
                         updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                         PRIMARY KEY(discord_id,guild_id)
                     );
                     CREATE TABLE predictions (
                         prediction_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL DEFAULT 0,
                         creator_id INTEGER NOT NULL,
                         question TEXT NOT NULL,
                         status TEXT NOT NULL DEFAULT 'open',
                         outcome TEXT,
                         channel_id INTEGER,
                         thread_id INTEGER,
                         embed_message_id INTEGER,
                         channel_message_id INTEGER,
                         close_message_id INTEGER,
                         resolution_votes TEXT,
                         created_at INTEGER NOT NULL,
                         closes_at INTEGER NOT NULL,
                         resolved_at INTEGER,
                         resolved_by INTEGER,
                         current_price INTEGER,
                         initial_fair INTEGER,
                         prev_price INTEGER,
                         last_refresh_at INTEGER,
                         lp_pnl INTEGER DEFAULT 0
                     );
                     CREATE INDEX idx_predictions_guild_status
                         ON predictions(guild_id,status);
                     CREATE TABLE prediction_levels (
                         level_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         prediction_id INTEGER NOT NULL,
                         side TEXT NOT NULL,
                         price INTEGER NOT NULL,
                         remaining_size INTEGER NOT NULL,
                         posted_at INTEGER NOT NULL
                     );
                     CREATE INDEX idx_pred_levels_pred_side_price
                         ON prediction_levels(prediction_id,side,price);
                     CREATE TABLE prediction_positions (
                         prediction_id INTEGER NOT NULL,
                         discord_id INTEGER NOT NULL,
                         yes_contracts INTEGER NOT NULL DEFAULT 0,
                         yes_cost_basis_total INTEGER NOT NULL DEFAULT 0,
                         no_contracts INTEGER NOT NULL DEFAULT 0,
                         no_cost_basis_total INTEGER NOT NULL DEFAULT 0,
                         bankruptcy_penalty INTEGER NOT NULL DEFAULT 0,
                         vanity_tax INTEGER NOT NULL DEFAULT 0,
                         PRIMARY KEY(prediction_id,discord_id)
                     );
                     CREATE INDEX idx_prediction_positions_user
                         ON prediction_positions(discord_id,prediction_id);
                     CREATE TABLE prediction_trades (
                         trade_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         prediction_id INTEGER NOT NULL,
                         discord_id INTEGER NOT NULL,
                         action TEXT NOT NULL,
                         contracts INTEGER NOT NULL,
                         jopacoins INTEGER NOT NULL,
                         vwap_x100 INTEGER NOT NULL,
                         last_fill_price INTEGER,
                         trade_time INTEGER NOT NULL
                     );
                     CREATE INDEX idx_pred_trades_pred_time
                         ON prediction_trades(prediction_id,trade_time);
                     CREATE TABLE prediction_fair_snapshots (
                         snapshot_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         market_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         snapshot_at INTEGER NOT NULL,
                         fair_pct INTEGER NOT NULL,
                         reason TEXT NOT NULL
                     );
                     CREATE INDEX idx_prediction_fair_snapshots_lookup
                         ON prediction_fair_snapshots(market_id,guild_id,snapshot_at);
                     CREATE TABLE app_kv (
                         guild_id INTEGER NOT NULL,
                         key TEXT NOT NULL,
                         value TEXT NOT NULL,
                         PRIMARY KEY(guild_id,key)
                     );
                     CREATE TABLE economy_daily_events (
                         event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL,
                         effects TEXT NOT NULL,
                         starts_at INTEGER NOT NULL,
                         ends_at INTEGER NOT NULL
                     );
                     CREATE TABLE economy_ledger_context (
                         id INTEGER PRIMARY KEY CHECK(id=1),
                         source TEXT,
                         actor_id INTEGER,
                         related_type TEXT,
                         related_id TEXT,
                         reason TEXT,
                         metadata TEXT
                     );
                     CREATE TABLE economy_ledger_entries (
                         ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL,
                         account_type TEXT NOT NULL,
                         account_id INTEGER,
                         delta INTEGER NOT NULL,
                         source TEXT,
                         related_type TEXT,
                         related_id TEXT
                     );",
                    )
                    .expect("create migrated prediction fixture schema template");
                drop(connection);
                std::fs::read(file.path()).expect("read prediction fixture schema template")
            });
            let mut file = NamedTempFile::new().expect("create prediction SQLite fixture");
            file.as_file_mut()
                .write_all(template)
                .expect("copy prediction fixture schema template");
            let repository = PredictionRepository::new(file.path());
            Self { file, repository }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.file.path()).expect("open raw fixture connection")
        }

        fn player(&self, discord_id: i64, guild_id: i64, balance: i64) {
            self.connection()
                .execute(
                    "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
                     VALUES (?1,?2,?3,?4)",
                    params![discord_id, guild_id, format!("user{discord_id}"), balance],
                )
                .expect("insert player");
        }

        fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
            self.connection()
                .query_row(
                    "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                    params![discord_id, guild_id],
                    |row| row.get(0),
                )
                .expect("read balance")
        }

        fn market(&self) -> i64 {
            self.repository
                .create_orderbook_prediction(
                    Some(GUILD),
                    999,
                    "Will this market work?",
                    50,
                    None,
                    &initial_levels(),
                )
                .expect("create market")
        }

        fn position(&self, prediction_id: i64, position: Position) {
            self.connection()
                .execute(
                    "INSERT INTO prediction_positions(
                         prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                         no_contracts,no_cost_basis_total)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        prediction_id,
                        position.discord_id,
                        position.yes_contracts,
                        position.yes_cost_basis_total,
                        position.no_contracts,
                        position.no_cost_basis_total
                    ],
                )
                .expect("insert position");
        }
    }

    fn initial_levels() -> Vec<NewLevel> {
        [50, 50, 50, 20, 15, 10, 5]
            .into_iter()
            .enumerate()
            .flat_map(|(index, size)| {
                let offset = 2 + i64::try_from(index).expect("small index");
                [
                    NewLevel::ask(50 + offset, size),
                    NewLevel::bid(50 - offset, size),
                ]
            })
            .collect()
    }

    fn position(prediction_id: i64, discord_id: i64, yes: i64, yes_basis: i64) -> Position {
        Position {
            prediction_id,
            discord_id,
            yes_contracts: yes,
            yes_cost_basis_total: yes_basis,
            ..Position::default()
        }
    }

    #[test]
    fn test_schema_has_orderbook_tables() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .repository
                .schema_contract()
                .expect("inspect schema")
        );
    }

    #[test]
    fn test_prediction_indexes_support_user_and_refresh_lookups() {
        let fixture = Fixture::new();
        let connection = fixture.connection();
        let position_plan: String = connection
            .query_row(
                "EXPLAIN QUERY PLAN SELECT prediction_id FROM prediction_positions
                 WHERE discord_id=12345",
                [],
                |row| row.get(3),
            )
            .expect("position query plan");
        let trade_plan: String = connection
            .query_row(
                "EXPLAIN QUERY PLAN SELECT COALESCE(SUM(contracts),0)
                 FROM prediction_trades WHERE prediction_id=1 AND trade_time>=1000",
                [],
                |row| row.get(3),
            )
            .expect("trade query plan");
        assert!(position_plan.contains("idx_prediction_positions_user"));
        assert!(trade_plan.contains("idx_pred_trades_pred_time"));
    }

    #[test]
    fn test_position_transfer_rejects_non_open_market() {
        let fixture = Fixture::new();
        fixture.player(2, GUILD, 1_000);
        let market = fixture.market();
        fixture.position(market, position(market, 1, 8, 24));
        fixture
            .repository
            .set_status(market, "locked")
            .expect("lock market");
        assert_eq!(
            fixture
                .repository
                .transfer_position_contracts(market, 1, 2, ContractSide::Yes, 4)
                .expect("attempt transfer"),
            None
        );
        assert_eq!(
            fixture
                .repository
                .get_position(market, 1)
                .expect("position"),
            Some(position(market, 1, 8, 24))
        );
    }

    #[test]
    fn test_position_transfer_rejects_unregistered_recipient() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture.position(market, position(market, 1, 8, 24));
        assert_eq!(
            fixture
                .repository
                .transfer_position_contracts(market, 1, 2, ContractSide::Yes, 4)
                .expect("unregistered transfer"),
            None
        );
        fixture.player(2, OTHER_GUILD, 1_000);
        assert_eq!(
            fixture
                .repository
                .transfer_position_contracts(market, 1, 2, ContractSide::Yes, 4)
                .expect("wrong guild transfer"),
            None
        );
        fixture.player(2, GUILD, 1_000);
        assert_eq!(
            fixture
                .repository
                .transfer_position_contracts(market, 1, 2, ContractSide::Yes, 4)
                .expect("registered transfer"),
            Some(4)
        );
        assert_eq!(
            fixture
                .repository
                .get_position(market, 2)
                .expect("recipient position")
                .expect("recipient row")
                .yes_cost_basis_total,
            12
        );
    }

    #[test]
    fn test_get_market_view_matches_point_reads_and_uses_one_connection() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture.position(market, position(market, 401, 7, 31));
        fixture
            .connection()
            .execute(
                "INSERT INTO prediction_trades(prediction_id,discord_id,action,contracts,
             jopacoins,vwap_x100,last_fill_price,trade_time)
             VALUES (?1,401,'buy_yes',5,26,5200,52,2000)",
                [market],
            )
            .expect("insert trade");
        let snapshot = fixture
            .repository
            .get_market_snapshot(market, Some(401), 5)
            .expect("snapshot")
            .expect("market");
        assert_eq!(snapshot.market.prediction_id, market);
        assert_eq!(
            snapshot.book,
            fixture.repository.get_book(market).expect("book")
        );
        assert_eq!(snapshot.viewer_position.expect("viewer").yes_contracts, 7);
        assert_eq!(snapshot.recent_trades.len(), 1);
    }

    #[test]
    fn test_get_market_view_handles_missing_and_market_scoped_viewers() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .repository
                .get_market_snapshot(999_999, Some(1), 5)
                .expect("missing snapshot")
                .is_none()
        );
        let market = fixture.market();
        fixture.position(market, position(market, 402, 0, 0));
        assert!(
            fixture
                .repository
                .get_market_snapshot(market, None, 5)
                .expect("anonymous snapshot")
                .expect("market")
                .viewer_position
                .is_none()
        );
        assert!(
            fixture
                .repository
                .get_market_snapshot(market, Some(402), 5)
                .expect("empty viewer snapshot")
                .expect("market")
                .viewer_position
                .is_none()
        );
    }

    #[test]
    fn test_get_market_view_reads_one_consistent_sqlite_snapshot() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture.position(market, position(market, 403, 2, 10));
        let snapshot = fixture
            .repository
            .get_market_snapshot(market, Some(403), 5)
            .expect("snapshot")
            .expect("market");
        assert_eq!(snapshot.book.current_price, snapshot.market.current_price);
        assert_eq!(snapshot.viewer_position.expect("position").yes_contracts, 2);
        assert_eq!(snapshot.volume_since_refresh, 0);
    }

    #[test]
    fn test_open_orderbook_listing_includes_book_and_recent_volume() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(
                market,
                &[
                    NewLevel::ask(55, 7),
                    NewLevel::ask(60, 8),
                    NewLevel::bid(43, 9),
                    NewLevel::bid(47, 4),
                ],
            )
            .expect("replace levels");
        fixture
            .connection()
            .execute(
                "INSERT INTO prediction_trades(prediction_id,discord_id,action,contracts,
             jopacoins,vwap_x100,last_fill_price,trade_time)
             VALUES (?1,1,'buy_yes',10,50,5000,50,?2)",
                params![market, super::unix_timestamp().expect("clock")],
            )
            .expect("insert trade");
        let markets = fixture
            .repository
            .list_open_markets(Some(GUILD), super::unix_timestamp().expect("clock"))
            .expect("list markets");
        assert_eq!(markets[0].top_ask, Some(55));
        assert_eq!(markets[0].top_bid, Some(47));
        assert_eq!(markets[0].volume_recent, 10);
    }

    #[test]
    fn test_open_orderbook_listing_uses_one_select_one_market() {
        listing_count_contract(1);
    }

    #[test]
    fn test_open_orderbook_listing_uses_one_select_five_markets() {
        listing_count_contract(5);
    }

    fn listing_count_contract(count: usize) {
        let fixture = Fixture::new();
        for _ in 0..count {
            fixture.market();
        }
        let listed = fixture
            .repository
            .list_open_markets(Some(GUILD), 1_000_000_000_000)
            .expect("single-query listing");
        assert_eq!(listed.len(), count);
    }

    #[test]
    fn test_buy_yes_top_of_book_only() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        let receipt = fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 3)
            .expect("buy yes");
        assert_eq!(receipt.fills, vec![(52, 3)]);
        assert_eq!(receipt.total, quote_total(3 * 52, true));
        assert_eq!(
            fixture
                .repository
                .get_position(market, 1)
                .expect("position")
                .expect("position row")
                .yes_contracts,
            3
        );
    }

    #[test]
    fn test_buy_yes_walks_deeper() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000_000);
        let market = fixture.market();
        let receipt = fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 53)
            .expect("sweep asks");
        assert_eq!(receipt.fills, vec![(52, 50), (53, 3)]);
    }

    #[test]
    fn test_buy_preview_walks_live_levels_without_mutating_book() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(
                market,
                &[
                    NewLevel::ask(40, 2),
                    NewLevel::ask(70, 3),
                    NewLevel::bid(35, 5),
                ],
            )
            .expect("replace levels");
        let before = fixture.repository.get_book(market).expect("book");
        let quote = fixture
            .repository
            .quote_buy_contracts(market, ContractSide::Yes, 5)
            .expect("quote");
        assert_eq!(quote.fills, vec![(40, 2), (70, 3)]);
        assert_eq!(fixture.repository.get_book(market).expect("book"), before);
    }

    #[test]
    fn test_buy_rejections() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000_000);
        fixture.player(2, GUILD, 10);
        fixture.player(3, GUILD, -50);
        let market = fixture.market();
        assert!(matches!(
            fixture
                .repository
                .buy_contracts_atomic(market, 1, ContractSide::Yes, 201),
            Err(PredictionRepositoryError::InsufficientDepth { .. })
        ));
        assert!(matches!(
            fixture
                .repository
                .buy_contracts_atomic(market, 2, ContractSide::Yes, 5),
            Err(PredictionRepositoryError::InsufficientBalance { .. })
        ));
        assert!(matches!(
            fixture
                .repository
                .buy_contracts_atomic(market, 1, ContractSide::Yes, 0),
            Err(PredictionRepositoryError::InvalidContracts)
        ));
        assert!(matches!(
            fixture
                .repository
                .buy_contracts_atomic(market, 3, ContractSide::Yes, 1),
            Err(PredictionRepositoryError::InDebt)
        ));
    }

    #[test]
    fn test_buy_no_mirrors_yes_bid() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        let receipt = fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::No, 3)
            .expect("buy no");
        assert_eq!(receipt.total, quote_total(3 * (100 - 48), true));
        assert_eq!(receipt.fills, vec![(48, 3)]);
    }

    #[test]
    fn test_sell_yes_proceeds_at_top_bid() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 3)
            .expect("buy");
        let receipt = fixture
            .repository
            .sell_contracts_atomic(market, 1, ContractSide::Yes, 2)
            .expect("sell");
        assert_eq!(receipt.total, quote_total(2 * 48, false));
        let remaining = fixture
            .repository
            .get_position(market, 1)
            .expect("position")
            .expect("remaining");
        assert_eq!(remaining.yes_contracts, 1);
        assert_eq!(remaining.yes_cost_basis_total, 6);
    }

    #[test]
    fn test_sell_yes_rejected_without_holdings() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        assert!(matches!(
            fixture
                .repository
                .sell_contracts_atomic(market, 1, ContractSide::Yes, 1),
            Err(PredictionRepositoryError::InsufficientHoldings { held: 0, .. })
        ));
    }

    #[test]
    fn test_sell_full_position_deletes_row() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 3)
            .expect("buy");
        fixture
            .repository
            .sell_contracts_atomic(market, 1, ContractSide::Yes, 3)
            .expect("sell all");
        assert!(
            fixture
                .repository
                .get_position(market, 1)
                .expect("position")
                .is_none()
        );
    }

    #[test]
    fn test_sell_missing_player_row_fails_cleanly() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture.position(market, position(market, 4_242, 5, 100));
        let before = fixture.repository.get_book(market).expect("book");
        assert!(matches!(
            fixture
                .repository
                .sell_contracts_atomic(market, 4_242, ContractSide::Yes, 2),
            Err(PredictionRepositoryError::PlayerNotFound)
        ));
        assert_eq!(fixture.repository.get_book(market).expect("book"), before);
        assert_eq!(
            fixture
                .repository
                .get_position(market, 4_242)
                .expect("position")
                .expect("row")
                .yes_contracts,
            5
        );
    }

    #[test]
    fn test_hedging_yes_and_no_held_independently() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 3)
            .expect("buy yes");
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::No, 2)
            .expect("buy no");
        let held = fixture
            .repository
            .get_position(market, 1)
            .expect("position")
            .expect("row");
        assert_eq!((held.yes_contracts, held.no_contracts), (3, 2));
    }

    #[test]
    fn test_refresh_prunes_legacy_inner_quotes_but_keeps_crossing_levels() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(
                market,
                &[
                    NewLevel::ask(49, 5),
                    NewLevel::ask(51, 5),
                    NewLevel::ask(52, 5),
                    NewLevel::bid(51, 5),
                    NewLevel::bid(49, 5),
                    NewLevel::bid(48, 5),
                ],
            )
            .expect("replace");
        fixture
            .repository
            .apply_refresh(market, 50, &[], 100, "refresh", 2)
            .expect("refresh");
        let book = fixture.repository.get_book(market).expect("book");
        assert!(book.yes_asks.contains(&(49, 5)));
        assert!(book.yes_bids.contains(&(51, 5)));
        assert!(!book.yes_asks.contains(&(51, 5)));
        assert!(!book.yes_bids.contains(&(49, 5)));
    }

    #[test]
    fn test_refresh_layers_size_onto_existing_levels() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .apply_refresh(
                market,
                50,
                &[NewLevel::ask(54, 10), NewLevel::bid(46, 10)],
                100,
                "refresh",
                2,
            )
            .expect("layer");
        let book = fixture.repository.get_book(market).expect("book");
        assert!(book.yes_asks.contains(&(54, 60)));
        assert!(book.yes_bids.contains(&(46, 60)));
    }

    #[test]
    fn test_refresh_quiet_market_accumulates_depth() {
        let fixture = Fixture::new();
        let market = fixture.market();
        for now in [100, 200] {
            fixture
                .repository
                .apply_refresh(market, 50, &[NewLevel::ask(60, 2)], now, "refresh", 2)
                .expect("layer");
        }
        assert!(
            fixture
                .repository
                .get_book(market)
                .expect("book")
                .yes_asks
                .contains(&(60, 4))
        );
    }

    #[test]
    fn test_refresh_locked_levels_at_book_boundaries_are_kept() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(market, &[NewLevel::ask(99, 4), NewLevel::bid(1, 5)])
            .expect("boundary book");
        fixture
            .repository
            .apply_refresh(market, 95, &[NewLevel::bid(91, 2)], 100, "refresh", 2)
            .expect("refresh");
        let book = fixture.repository.get_book(market).expect("book");
        assert!(book.yes_asks.contains(&(99, 4)));
        assert!(book.yes_bids.contains(&(1, 5)));
    }

    #[test]
    fn test_refresh_preserves_crossing_ask_for_arb() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(market, &[NewLevel::ask(49, 5)])
            .expect("crossing ask");
        fixture
            .repository
            .apply_refresh(market, 50, &[NewLevel::ask(54, 10)], 100, "refresh", 2)
            .expect("refresh");
        assert!(
            fixture
                .repository
                .get_book(market)
                .expect("book")
                .yes_asks
                .contains(&(49, 5))
        );
    }

    #[test]
    fn test_get_markets_due_for_refresh() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .connection()
            .execute(
                "UPDATE predictions SET last_refresh_at=0 WHERE prediction_id=?1",
                [market],
            )
            .expect("age market");
        let due = fixture
            .repository
            .markets_due_for_refresh(1_000_000_000_000)
            .expect("due markets");
        assert!(
            due.iter()
                .any(|candidate| candidate.prediction_id == market)
        );
    }

    #[test]
    fn test_resolve_yes_pays_yes_holders() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        fixture.player(2, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 5)
            .expect("buy yes");
        fixture
            .repository
            .buy_contracts_atomic(market, 2, ContractSide::No, 4)
            .expect("buy no");
        let before_yes = fixture.balance(1, GUILD);
        let before_no = fixture.balance(2, GUILD);
        let settlement = fixture
            .repository
            .settle_orderbook(market, ContractSide::Yes, Some(12_345))
            .expect("settle");
        assert_eq!(settlement.total_payout, 5 * CONTRACT_VALUE);
        assert_eq!(fixture.balance(1, GUILD) - before_yes, 5 * CONTRACT_VALUE);
        assert_eq!(fixture.balance(2, GUILD), before_no);
        assert_eq!(
            fixture
                .repository
                .get_prediction(market)
                .expect("market")
                .expect("row")
                .resolved_by,
            Some(12_345)
        );
    }

    #[test]
    fn test_apply_refresh_skips_resolved_market() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .settle_orderbook(market, ContractSide::Yes, None)
            .expect("settle empty market");
        assert!(
            !fixture
                .repository
                .apply_refresh(market, 70, &[NewLevel::ask(71, 5)], 100, "refresh", 2)
                .expect("skip")
        );
        assert_eq!(
            fixture
                .repository
                .get_prediction(market)
                .expect("market")
                .expect("row")
                .current_price,
            Some(50)
        );
    }

    #[test]
    fn test_cancel_refunds_cost_basis() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        fixture.player(2, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 3)
            .expect("buy yes");
        fixture
            .repository
            .buy_contracts_atomic(market, 2, ContractSide::No, 2)
            .expect("buy no");
        let before = fixture.balance(1, GUILD) + fixture.balance(2, GUILD);
        let refunded = fixture.repository.cancel_orderbook(market).expect("cancel");
        assert_eq!(
            fixture.balance(1, GUILD) + fixture.balance(2, GUILD),
            before + refunded
        );
        assert_eq!(
            refunded,
            quote_total(3 * 52, true) + quote_total(2 * 52, true)
        );
    }

    #[test]
    fn test_cancel_with_round_trip_keeps_realized_pnl() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 5)
            .expect("buy");
        fixture
            .repository
            .sell_contracts_atomic(market, 1, ContractSide::Yes, 2)
            .expect("sell");
        let before_cancel = fixture.balance(1, GUILD);
        let refund = fixture.repository.cancel_orderbook(market).expect("cancel");
        assert_eq!(fixture.balance(1, GUILD), before_cancel + refund);
        assert_eq!(
            refund,
            quote_total(5 * 52, true) - quote_total(5 * 52, true) * 2 / 5
        );
    }

    #[test]
    fn test_get_user_open_positions_returns_open_only() {
        let fixture = Fixture::new();
        let open = fixture.market();
        let resolved = fixture.market();
        fixture.position(open, position(open, 1, 2, 10));
        fixture.position(resolved, position(resolved, 1, 1, 5));
        fixture
            .repository
            .settle_orderbook(resolved, ContractSide::No, None)
            .expect("settle");
        let positions = fixture
            .repository
            .get_user_open_positions(1, Some(GUILD))
            .expect("positions");
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].position.prediction_id, open);
    }

    #[test]
    fn test_get_user_open_positions_includes_top_active_marks() {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(
                market,
                &[
                    NewLevel::bid(43, 8),
                    NewLevel::bid(47, 3),
                    NewLevel::ask(54, 6),
                    NewLevel::ask(60, 9),
                ],
            )
            .expect("book");
        fixture.position(
            market,
            Position {
                prediction_id: market,
                discord_id: 1,
                yes_contracts: 3,
                yes_cost_basis_total: 150,
                no_contracts: 4,
                no_cost_basis_total: 200,
            },
        );
        let positions = fixture
            .repository
            .get_user_open_positions(1, Some(GUILD))
            .expect("positions");
        assert_eq!(
            (positions[0].yes_mark, positions[0].no_mark),
            (Some(47), Some(46))
        );
    }

    #[test]
    fn test_get_user_open_positions_marks_missing_book_side_as_none_asks_only() {
        missing_mark_contract(
            &[NewLevel::ask(61, 5), NewLevel::bid(44, 0)],
            None,
            Some(39),
        );
    }

    #[test]
    fn test_get_user_open_positions_marks_missing_book_side_as_none_bids_only() {
        missing_mark_contract(
            &[NewLevel::bid(44, 5), NewLevel::ask(61, 0)],
            Some(44),
            None,
        );
    }

    fn missing_mark_contract(levels: &[NewLevel], yes: Option<i64>, no: Option<i64>) {
        let fixture = Fixture::new();
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(market, levels)
            .expect("book");
        fixture.position(
            market,
            Position {
                prediction_id: market,
                discord_id: 1,
                yes_contracts: 3,
                yes_cost_basis_total: 150,
                no_contracts: 4,
                no_cost_basis_total: 200,
            },
        );
        let marked = fixture
            .repository
            .get_user_open_positions(1, Some(GUILD))
            .expect("positions");
        assert_eq!((marked[0].yes_mark, marked[0].no_mark), (yes, no));
    }

    #[test]
    fn test_guild_net_worth_leaderboard_uses_current_prices_for_active_positions() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 100);
        fixture.player(2, GUILD, 200);
        fixture.player(3, GUILD, 500);
        let market = fixture.market();
        fixture
            .connection()
            .execute(
                "UPDATE predictions SET current_price=70 WHERE prediction_id=?1",
                [market],
            )
            .expect("move fair");
        fixture.position(
            market,
            Position {
                prediction_id: market,
                discord_id: 1,
                yes_contracts: 3,
                no_contracts: 2,
                ..Position::default()
            },
        );
        fixture.position(
            market,
            Position {
                prediction_id: market,
                discord_id: 2,
                no_contracts: 5,
                ..Position::default()
            },
        );
        let leaderboard = fixture
            .repository
            .net_worth_leaderboard(Some(GUILD))
            .expect("leaderboard");
        assert_eq!(
            leaderboard
                .iter()
                .map(|row| row.discord_id)
                .collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert_eq!(leaderboard[1].net_worth, 215);
        assert_eq!(leaderboard[2].net_worth, 127);
    }

    #[test]
    fn test_create_orderbook_writes_initial_fair_snapshot() {
        let fixture = Fixture::new();
        let market = fixture.market();
        let history = fixture
            .repository
            .fair_history(market, Some(GUILD))
            .expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].1, 50);
    }

    #[test]
    fn test_pop_one_shot_flag_self_clears() {
        let fixture = Fixture::new();
        fixture
            .connection()
            .execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,'banner','0')",
                [GUILD],
            )
            .expect("seed flag");
        assert!(
            fixture
                .repository
                .pop_one_shot_flag(Some(GUILD), "banner")
                .expect("first")
        );
        assert!(
            !fixture
                .repository
                .pop_one_shot_flag(Some(GUILD), "banner")
                .expect("second")
        );
        assert!(
            !fixture
                .repository
                .pop_one_shot_flag(Some(GUILD), "missing")
                .expect("missing")
        );
    }

    #[test]
    fn test_fair_history_backfill_from_levels_groups_by_utc_day() {
        let fixture = Fixture::new();
        let market = fixture.market();
        let connection = fixture.connection();
        connection
            .execute(
                "DELETE FROM prediction_fair_snapshots WHERE market_id=?1",
                [market],
            )
            .expect("clear history");
        connection
            .execute(
                "DELETE FROM prediction_levels WHERE prediction_id=?1",
                [market],
            )
            .expect("clear levels");
        connection.execute_batch(&format!(
            "INSERT INTO prediction_levels(prediction_id,side,price,remaining_size,posted_at)
             VALUES ({market},'yes_ask',65,100,1775044800),
                    ({market},'yes_bid',35,100,1775044800),
                    ({market},'yes_ask',70,100,1775207400);
             INSERT INTO prediction_fair_snapshots(market_id,guild_id,snapshot_at,fair_pct,reason)
             SELECT l.prediction_id,p.guild_id,
                    CAST(strftime('%s',date(l.posted_at,'unixepoch'),'+1 day','-1 second') AS INTEGER),
                    CASE WHEN MIN(CASE WHEN l.side='yes_ask' THEN l.price END) IS NOT NULL
                              AND MAX(CASE WHEN l.side='yes_bid' THEN l.price END) IS NOT NULL
                         THEN (MIN(CASE WHEN l.side='yes_ask' THEN l.price END)
                              +MAX(CASE WHEN l.side='yes_bid' THEN l.price END))/2
                         ELSE MIN(CASE WHEN l.side='yes_ask' THEN l.price END) END,
                    'backfill_levels'
             FROM prediction_levels l JOIN predictions p ON p.prediction_id=l.prediction_id
             GROUP BY l.prediction_id,date(l.posted_at,'unixepoch');"
        )).expect("run test-only backfill fixture");
        let history = fixture
            .repository
            .fair_history(market, Some(GUILD))
            .expect("history");
        assert_eq!(
            history.iter().map(|row| row.1).collect::<Vec<_>>(),
            vec![50, 70]
        );
    }

    #[test]
    fn test_get_user_orderbook_stats_realized_pnl() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 100_000);
        assert_eq!(
            fixture
                .repository
                .user_orderbook_stats(1, Some(GUILD))
                .expect("empty stats")
                .resolved_markets,
            0
        );
        let win = fixture.market();
        let loss = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(win, 1, ContractSide::Yes, 3)
            .expect("win position");
        fixture
            .repository
            .buy_contracts_atomic(loss, 1, ContractSide::Yes, 3)
            .expect("loss position");
        fixture
            .repository
            .settle_orderbook(win, ContractSide::Yes, None)
            .expect("win settle");
        fixture
            .repository
            .settle_orderbook(loss, ContractSide::No, None)
            .expect("loss settle");
        let stats = fixture
            .repository
            .user_orderbook_stats(1, Some(GUILD))
            .expect("stats");
        assert_eq!(
            (stats.resolved_markets, stats.wins, stats.losses),
            (2, 1, 1)
        );
    }

    #[test]
    fn test_get_player_orderbook_pnl_history() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 100_000);
        let resolved = fixture.market();
        let open = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(resolved, 1, ContractSide::Yes, 3)
            .expect("resolved position");
        fixture
            .repository
            .buy_contracts_atomic(open, 1, ContractSide::Yes, 3)
            .expect("open position");
        fixture
            .repository
            .settle_orderbook(resolved, ContractSide::Yes, None)
            .expect("settle");
        let history = fixture
            .repository
            .player_pnl_history(1, Some(GUILD))
            .expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].prediction_id, resolved);
        assert!(history[0].settle_time > 0);
    }

    #[test]
    fn test_atomic_buy_does_not_oversell_shared_depth() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        fixture.player(2, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .replace_levels(market, &[NewLevel::ask(52, 1)])
            .expect("one contract book");
        let repository = Arc::new(fixture.repository.clone());
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for discord_id in [1, 2] {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                repository.buy_contracts_atomic(market, discord_id, ContractSide::Yes, 1)
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("buyer thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(PredictionRepositoryError::InsufficientDepth { .. })
                ))
                .count(),
            1
        );
    }

    #[test]
    fn test_none_guild_normalizes_to_zero_and_signed_ids_round_trip() {
        let fixture = Fixture::new();
        let market = fixture
            .repository
            .create_orderbook_prediction(None, -9, "Signed identifiers survive", 50, Some(-11), &[])
            .expect("create");
        let stored = fixture
            .repository
            .get_prediction(market)
            .expect("get")
            .expect("row");
        assert_eq!(stored.guild_id, 0);
        assert_eq!(stored.creator_id, -9);
        assert_eq!(stored.channel_id, Some(-11));
    }

    #[test]
    fn test_quote_total_rounding_core_boundaries() {
        assert_eq!(quote_total(205, true), 21);
        assert_eq!(quote_total(205, false), 20);
        assert_eq!(quote_total(4, true), 1);
        assert_eq!(quote_total(4, false), 0);
    }

    #[test]
    fn test_book_side_strings_match_python_storage() {
        assert_eq!(BookSide::YesAsk.as_str(), "yes_ask");
        assert_eq!(BookSide::YesBid.as_str(), "yes_bid");
    }

    #[test]
    fn command_queries_and_configured_trade_limit_match_persistent_surface() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .update_discord_ids(market, Some(8_001), Some(8_002), Some(8_003))
            .expect("store Discord identity");
        assert_eq!(
            fixture
                .repository
                .get_prediction_by_thread(8_001)
                .expect("thread lookup")
                .expect("market")
                .prediction_id,
            market
        );
        assert_eq!(
            fixture
                .repository
                .list_markets_by_status(Some(GUILD), "open")
                .expect("status list")
                .len(),
            1
        );
        assert!(matches!(
            fixture
                .repository
                .quote_buy_contracts_with_max(market, ContractSide::Yes, 3, 2),
            Err(PredictionRepositoryError::TradeTooLarge { maximum: 2 })
        ));
        fixture
            .repository
            .buy_contracts_atomic_with_max(market, 1, ContractSide::Yes, 3, 7)
            .expect("configured trade limit permits buy");
        let positions = fixture
            .repository
            .get_user_open_position_summaries(1, Some(GUILD))
            .expect("position summaries");
        assert_eq!(positions[0].question, "Will this market work?");
        assert_eq!(positions[0].marked.position.yes_contracts, 3);
    }

    #[test]
    fn gamba_penalty_and_event_effects_match_python_fail_soft_policy() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 0);
        assert!(
            fixture
                .repository
                .debit_gamba_channel_penalty(1, Some(GUILD))
                .expect("debit")
        );
        assert_eq!(fixture.balance(1, GUILD), -1);
        assert!(
            !fixture
                .repository
                .debit_gamba_channel_penalty(99, Some(GUILD))
                .expect("missing player is no-op")
        );
        fixture
            .connection()
            .execute(
                "INSERT INTO economy_daily_events(guild_id,effects,starts_at,ends_at)
                 VALUES (?1,?2,100,200)",
                params![
                    GUILD,
                    r#"{"prediction_payout_multiplier":1.025,"prediction_spread_ticks_delta":-2}"#
                ],
            )
            .expect("event");
        let effects = fixture
            .repository
            .active_prediction_effects(Some(GUILD), 150, true)
            .expect("effects");
        assert_eq!(effects.payout_multiplier_bps, 10_250);
        assert_eq!(effects.spread_ticks_delta, -2);
        assert_eq!(
            fixture
                .repository
                .active_prediction_effects(Some(GUILD), 150, false)
                .expect("disabled effects")
                .payout_multiplier_bps,
            10_000
        );
    }

    #[test]
    fn detailed_cancel_returns_every_holder_and_cost_basis_refund() {
        let fixture = Fixture::new();
        fixture.player(1, GUILD, 1_000);
        fixture.player(2, GUILD, 1_000);
        let market = fixture.market();
        fixture
            .repository
            .buy_contracts_atomic(market, 1, ContractSide::Yes, 3)
            .expect("first buy");
        fixture
            .repository
            .buy_contracts_atomic(market, 2, ContractSide::No, 2)
            .expect("second buy");
        let result = fixture
            .repository
            .cancel_orderbook_detailed(market)
            .expect("cancel");
        assert_eq!(result.refunded.len(), 2);
        assert_eq!(
            result.total_refunded,
            result.refunded.iter().map(|row| row.refund).sum::<i64>()
        );
    }

    #[test]
    #[ignore = "requires CAMA_PREDICTION_DB_PATH pointing at a disposable dev-DB clone"]
    fn test_existing_database_prediction_read_smoke() {
        let path = std::env::var_os("CAMA_PREDICTION_DB_PATH")
            .expect("set CAMA_PREDICTION_DB_PATH to a disposable clone");
        let repository = PredictionRepository::new(path);
        assert!(
            repository
                .schema_contract()
                .expect("inspect migrated dev DB")
        );
        for guild_id in [None, Some(0)] {
            let markets = repository
                .list_open_markets(guild_id, super::unix_timestamp().expect("clock"))
                .expect("read open market summaries");
            if let Some(market) = markets.first() {
                let snapshot = repository
                    .get_market_snapshot(market.market.prediction_id, None, 5)
                    .expect("read market snapshot")
                    .expect("listed market still exists");
                assert_eq!(snapshot.market.guild_id, market.market.guild_id);
            }
        }
    }
}
