//! Existing-schema SQLite persistence for daily economy events.
//!
//! Python remains the migration authority while the Rust implementation runs
//! alongside it.  This repository therefore opens existing database files
//! without SQLite's create flag and never creates or alters caller schema.
//! Event insertion and every direct balance leg share one `BEGIN IMMEDIATE`
//! transaction, preserving Python's one-card-per-guild/day idempotency and its
//! trigger-backed economy-ledger context.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

const SECONDS_PER_DAY: i64 = 86_400;
const DEFAULT_TREND_WINDOWS: [i64; 3] = [1, 3, 7];
const DEFAULT_FLOW_WINDOWS: [i64; 2] = [3, 7];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyMode {
    Recovery,
    Normal,
    Disabled,
}

impl PolicyMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recovery => "recovery",
            Self::Normal => "normal",
            Self::Disabled => "disabled",
        }
    }

    fn from_database(value: &str) -> Result<Self, EconomyEventRepositoryError> {
        match value {
            "recovery" => Ok(Self::Recovery),
            "normal" => Ok(Self::Normal),
            "disabled" => Ok(Self::Disabled),
            other => Err(EconomyEventRepositoryError::InvalidDatabaseValue {
                field: "economy_policy_state.mode",
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PolicyState {
    pub guild_id: i64,
    pub mode: PolicyMode,
    pub target_annual_rate: f64,
    pub inflation_ceiling: f64,
    pub recovery_started_at: Option<i64>,
    pub stable_since: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BalanceSheet {
    pub player_wallets: i64,
    pub positive_wallets: i64,
    pub visible_debt: i64,
    pub player_count: usize,
    pub average_wallet: f64,
    pub reserve_available: i64,
    pub reserve_locked: i64,
    pub reserve_next_match_pot: i64,
    pub prediction_open_cash: i64,
    pub wager_escrow: i64,
    pub monetary_stock: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub guild_id: i64,
    pub snapshot_date: String,
    pub captured_at: i64,
    pub balance_sheet: BalanceSheet,
    pub annualized_30d: Option<f64>,
    pub annualized_90d: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonetaryTrend {
    pub requested_days: i64,
    pub elapsed_days: f64,
    pub anchor_date: String,
    pub anchor_stock: i64,
    pub current_stock: i64,
    pub change_jc: i64,
    pub period_rate: f64,
    pub daily_rate: f64,
    pub annualized_rate: f64,
    pub average_daily_change_jc: f64,
}

pub type MonetaryTrends = BTreeMap<i64, Option<MonetaryTrend>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowIndicator {
    pub days: i64,
    pub net_flow_jc: i64,
    pub gross_flow_jc: i64,
    pub inflow_jc: i64,
    pub outflow_jc: i64,
    pub entry_count: usize,
    pub active_players: usize,
    pub active_player_rate: f64,
    pub average_daily_net_jc: f64,
    pub average_daily_gross_jc: f64,
    pub daily_turnover_rate: f64,
}

pub type FlowIndicators = BTreeMap<i64, FlowIndicator>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DistributionIndicators {
    pub positive_player_count: usize,
    pub median_positive_wallet: f64,
    pub top_decile_count: usize,
    pub top_decile_share: f64,
    pub gini: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceVolumes {
    pub reward_credits: f64,
    pub gamba_credits: f64,
    pub gamba_debits: f64,
    pub bet_payouts: f64,
    pub prediction_payouts: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventDirection {
    Deflationary,
    Neutral,
    Boon,
}

impl EventDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deflationary => "deflationary",
            Self::Neutral => "neutral",
            Self::Boon => "boon",
        }
    }

    fn from_database(value: &str) -> Result<Self, EconomyEventRepositoryError> {
        match value {
            "deflationary" => Ok(Self::Deflationary),
            "neutral" => Ok(Self::Neutral),
            "boon" => Ok(Self::Boon),
            other => Err(EconomyEventRepositoryError::InvalidDatabaseValue {
                field: "economy_daily_events.direction",
                value: other.to_owned(),
            }),
        }
    }
}

/// Fixed, typed view of the JSON payload shared with Python.
///
/// Missing or corrupt fields read as neutral values. Legacy persisted depth
/// modifiers remain visible in `effects_json`, but consumers deliberately see
/// a neutral `prediction_depth_multiplier`, matching Python's public model.
#[derive(Clone, Debug, PartialEq)]
pub struct EventEffects {
    pub reward_multiplier: f64,
    pub gamba_win_multiplier: f64,
    pub gamba_loss_multiplier: f64,
    pub bet_payout_multiplier: f64,
    pub prediction_payout_multiplier: f64,
    pub prediction_depth_multiplier: f64,
    pub prediction_spread_ticks_delta: i64,
    pub reserve_burn_jc: i64,
    pub reserve_release_jc: i64,
    pub wallet_burn_rate: f64,
    pub wallet_burn_jc: i64,
    pub reserve_voting_disabled: bool,
}

impl Default for EventEffects {
    fn default() -> Self {
        Self {
            reward_multiplier: 1.0,
            gamba_win_multiplier: 1.0,
            gamba_loss_multiplier: 1.0,
            bet_payout_multiplier: 1.0,
            prediction_payout_multiplier: 1.0,
            prediction_depth_multiplier: 1.0,
            prediction_spread_ticks_delta: 0,
            reserve_burn_jc: 0,
            reserve_release_jc: 0,
            wallet_burn_rate: 0.0,
            wallet_burn_jc: 0,
            reserve_voting_disabled: false,
        }
    }
}

impl EventEffects {
    /// Deterministic JSON with the same sorted-key shape as Python's
    /// `json.dumps(..., sort_keys=True)`.
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                "{{\"bet_payout_multiplier\": {}, ",
                "\"gamba_loss_multiplier\": {}, ",
                "\"gamba_win_multiplier\": {}, ",
                "\"prediction_depth_multiplier\": {}, ",
                "\"prediction_payout_multiplier\": {}, ",
                "\"prediction_spread_ticks_delta\": {}, ",
                "\"reserve_burn_jc\": {}, ",
                "\"reserve_release_jc\": {}, ",
                "\"reserve_voting_disabled\": {}, ",
                "\"reward_multiplier\": {}, ",
                "\"wallet_burn_jc\": {}, ",
                "\"wallet_burn_rate\": {}}}"
            ),
            float_json(self.bet_payout_multiplier),
            float_json(self.gamba_loss_multiplier),
            float_json(self.gamba_win_multiplier),
            float_json(self.prediction_depth_multiplier),
            float_json(self.prediction_payout_multiplier),
            self.prediction_spread_ticks_delta,
            self.reserve_burn_jc,
            self.reserve_release_jc,
            self.reserve_voting_disabled,
            float_json(self.reward_multiplier),
            self.wallet_burn_jc,
            float_json(self.wallet_burn_rate),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventDraft {
    pub event_date: String,
    pub name: String,
    pub hero: String,
    pub direction: EventDirection,
    pub severity: i64,
    pub target_effect_jc: i64,
    pub forecast_flow_jc: i64,
    pub expected_effect_jc: i64,
    pub monetary_stock_before: i64,
    pub effects: EventEffects,
    pub announcement: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EconomyEvent {
    pub event_id: i64,
    pub guild_id: i64,
    pub event_date: String,
    pub name: String,
    pub hero: String,
    pub direction: EventDirection,
    pub severity: i64,
    pub target_effect_jc: i64,
    pub forecast_flow_jc: i64,
    pub expected_effect_jc: i64,
    pub direct_effect_jc: i64,
    pub actual_stock_change_jc: Option<i64>,
    pub monetary_stock_before: i64,
    pub effects_json: String,
    pub effects: EventEffects,
    /// Whether persisted JSON explicitly supplied the voting flag. Legacy
    /// rows omit it and presentation must derive the severe-edict fallback.
    pub reserve_voting_disabled_explicit: bool,
    pub announcement: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub created_at: i64,
    pub announced_at: Option<i64>,
    pub reminder_announced_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActivationResult {
    pub event: EconomyEvent,
    pub created: bool,
}

#[derive(Debug, Error)]
pub enum EconomyEventRepositoryError {
    #[error("economy-event SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid {field} value in existing database: {value:?}")]
    InvalidDatabaseValue { field: &'static str, value: String },
}

#[derive(Clone, Debug)]
pub struct EconomyEventRepository {
    path: PathBuf,
}

impl EconomyEventRepository {
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

    pub fn ensure_policy_state(
        &self,
        guild_id: Option<i64>,
        mode: PolicyMode,
        target_annual_rate: f64,
        inflation_ceiling: f64,
        now: i64,
    ) -> Result<PolicyState, EconomyEventRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO economy_policy_state (
                 guild_id, mode, target_annual_rate, inflation_ceiling,
                 recovery_started_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(guild_id) DO UPDATE SET
                 mode=excluded.mode,
                 target_annual_rate=excluded.target_annual_rate,
                 inflation_ceiling=excluded.inflation_ceiling,
                 recovery_started_at=CASE
                     WHEN excluded.mode='recovery'
                      AND economy_policy_state.mode!='recovery'
                         THEN excluded.recovery_started_at
                     ELSE economy_policy_state.recovery_started_at
                 END,
                 updated_at=excluded.updated_at",
            params![
                guild_id,
                mode.as_str(),
                target_annual_rate,
                inflation_ceiling,
                (mode == PolicyMode::Recovery).then_some(now),
                now,
            ],
        )?;
        self.get_policy_state(Some(guild_id))?.ok_or_else(|| {
            EconomyEventRepositoryError::InvalidDatabaseValue {
                field: "economy_policy_state.guild_id",
                value: guild_id.to_string(),
            }
        })
    }

    pub fn get_policy_state(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<PolicyState>, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT guild_id, mode, target_annual_rate, inflation_ceiling,
                        recovery_started_at, stable_since, updated_at
                 FROM economy_policy_state WHERE guild_id=?1",
                [Self::normalize_guild_id(guild_id)],
                |row| {
                    let mode: String = row.get(1)?;
                    Ok((
                        row.get(0)?,
                        mode,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?
            .map_or(Ok(None), |row| {
                Ok(Some(PolicyState {
                    guild_id: row.0,
                    mode: PolicyMode::from_database(&row.1)?,
                    target_annual_rate: row.2,
                    inflation_ceiling: row.3,
                    recovery_started_at: row.4,
                    stable_since: row.5,
                    updated_at: row.6,
                }))
            })
    }

    pub fn capture_balance_sheet(
        &self,
        guild_id: Option<i64>,
    ) -> Result<BalanceSheet, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let raw = connection.query_row(
            "WITH player_totals AS (
                 SELECT COUNT(*) AS player_count,
                        COALESCE(SUM(jopacoin_balance),0) AS player_wallets,
                        COALESCE(SUM(CASE WHEN jopacoin_balance>0
                            THEN jopacoin_balance ELSE 0 END),0) AS positive_wallets,
                        COALESCE(SUM(CASE WHEN jopacoin_balance<0
                            THEN -jopacoin_balance ELSE 0 END),0) AS visible_debt
                 FROM players WHERE guild_id=:gid
             ), reserve_totals AS (
                 SELECT COALESCE(total_collected,0) AS reserve_available,
                        COALESCE(next_match_pot,0) AS reserve_next_match_pot,
                        COALESCE(first_game_open_pool,0)
                          + COALESCE(first_game_lowskill_pool,0)
                            AS reserve_first_game_pools
                 FROM nonprofit_fund WHERE guild_id=:gid
             ), locked_totals AS (
                 SELECT COALESCE(SUM(fund_amount),0) AS reserve_locked
                 FROM disburse_proposals
                 WHERE guild_id=:gid AND status='active'
             ), first_game_claim_totals AS (
                 SELECT COALESCE(SUM(c.amount),0) AS active_claims
                 FROM first_game_pool_claims c
                 JOIN pending_matches pm
                   ON pm.guild_id=c.guild_id
                  AND pm.pending_match_id=c.pending_match_id
                 WHERE c.guild_id=:gid AND c.settled=0
             ), prediction_totals AS (
                 SELECT COALESCE(SUM(lp_pnl),0) AS prediction_open_cash
                 FROM predictions
                 WHERE guild_id=:gid AND status IN ('open','locked')
             ), duel_totals AS (
                 SELECT COALESCE(SUM(CASE
                     WHEN status='accepted' THEN wager*2
                     WHEN status='pending' THEN wager
                     ELSE 0 END),0) AS duel_escrow
                 FROM duel_challenges WHERE guild_id=:gid
             ), mafia_totals AS (
                 SELECT COALESCE(SUM(entry_fee*roster_size),0) AS mafia_escrow
                 FROM mafia_games
                 WHERE guild_id=:gid AND status='ACTIVE' AND phase!='RESOLVED'
             ), bet_totals AS (
                 SELECT COALESCE(SUM(b.amount*COALESCE(b.leverage,1)),0) AS bet_escrow
                 FROM bets b
                 JOIN pending_matches pm
                   ON pm.pending_match_id=b.pending_match_id
                  AND pm.guild_id=b.guild_id
                 WHERE b.guild_id=:gid
             )
             SELECT p.player_count, p.player_wallets, p.positive_wallets,
                    p.visible_debt, COALESCE(r.reserve_available,0),
                    l.reserve_locked+COALESCE(r.reserve_first_game_pools,0)
                      +fc.active_claims,
                    COALESCE(r.reserve_next_match_pot,0), pr.prediction_open_cash,
                    d.duel_escrow+m.mafia_escrow+b.bet_escrow
             FROM player_totals p
             CROSS JOIN locked_totals l
             CROSS JOIN first_game_claim_totals fc
             CROSS JOIN prediction_totals pr
             CROSS JOIN duel_totals d
             CROSS JOIN mafia_totals m
             CROSS JOIN bet_totals b
             LEFT JOIN reserve_totals r ON 1=1",
            rusqlite::named_params! {":gid": guild_id},
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )?;
        let player_count = usize::try_from(raw.0).unwrap_or_default();
        let average_wallet = if player_count == 0 {
            0.0
        } else {
            raw.1 as f64 / player_count as f64
        };
        Ok(BalanceSheet {
            player_wallets: raw.1,
            positive_wallets: raw.2,
            visible_debt: raw.3,
            player_count,
            average_wallet,
            reserve_available: raw.4,
            reserve_locked: raw.5,
            reserve_next_match_pot: raw.6,
            prediction_open_cash: raw.7,
            wager_escrow: raw.8,
            monetary_stock: raw.1 + raw.4 + raw.5 + raw.6 + raw.7 + raw.8,
        })
    }

    pub fn save_snapshot(
        &self,
        guild_id: Option<i64>,
        snapshot_date: &str,
        balance_sheet: &BalanceSheet,
        captured_at: i64,
    ) -> Result<Snapshot, EconomyEventRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let annualized_30d = annualized_change(
            &connection,
            guild_id,
            snapshot_date,
            balance_sheet.monetary_stock,
            30,
        )?;
        let annualized_90d = annualized_change(
            &connection,
            guild_id,
            snapshot_date,
            balance_sheet.monetary_stock,
            90,
        )?;
        connection.execute(
            "INSERT INTO economy_daily_snapshots (
                 guild_id, snapshot_date, captured_at, player_wallets,
                 positive_wallets, visible_debt, player_count, average_wallet,
                 reserve_available, reserve_locked, reserve_next_match_pot,
                 prediction_open_cash, wager_escrow, monetary_stock,
                 annualized_30d, annualized_90d
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(guild_id,snapshot_date) DO UPDATE SET
                 captured_at=excluded.captured_at,
                 player_wallets=excluded.player_wallets,
                 positive_wallets=excluded.positive_wallets,
                 visible_debt=excluded.visible_debt,
                 player_count=excluded.player_count,
                 average_wallet=excluded.average_wallet,
                 reserve_available=excluded.reserve_available,
                 reserve_locked=excluded.reserve_locked,
                 reserve_next_match_pot=excluded.reserve_next_match_pot,
                 prediction_open_cash=excluded.prediction_open_cash,
                 wager_escrow=excluded.wager_escrow,
                 monetary_stock=excluded.monetary_stock,
                 annualized_30d=excluded.annualized_30d,
                 annualized_90d=excluded.annualized_90d",
            params![
                guild_id,
                snapshot_date,
                captured_at,
                balance_sheet.player_wallets,
                balance_sheet.positive_wallets,
                balance_sheet.visible_debt,
                i64::try_from(balance_sheet.player_count).unwrap_or(i64::MAX),
                balance_sheet.average_wallet,
                balance_sheet.reserve_available,
                balance_sheet.reserve_locked,
                balance_sheet.reserve_next_match_pot,
                balance_sheet.prediction_open_cash,
                balance_sheet.wager_escrow,
                balance_sheet.monetary_stock,
                annualized_30d,
                annualized_90d,
            ],
        )?;
        Ok(Snapshot {
            guild_id,
            snapshot_date: snapshot_date.to_owned(),
            captured_at,
            balance_sheet: balance_sheet.clone(),
            annualized_30d,
            annualized_90d,
        })
    }

    pub fn get_latest_snapshot(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<Snapshot>, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT guild_id, snapshot_date, captured_at, player_wallets,
                        positive_wallets, visible_debt, player_count, average_wallet,
                        reserve_available, reserve_locked, reserve_next_match_pot,
                        prediction_open_cash, wager_escrow, monetary_stock,
                        annualized_30d, annualized_90d
                 FROM economy_daily_snapshots
                 WHERE guild_id=?1 ORDER BY snapshot_date DESC LIMIT 1",
                [Self::normalize_guild_id(guild_id)],
                row_to_snapshot,
            )
            .optional()?)
    }

    pub fn get_monetary_trends(
        &self,
        guild_id: Option<i64>,
        current_stock: i64,
        now: i64,
    ) -> Result<MonetaryTrends, EconomyEventRepositoryError> {
        self.get_monetary_trends_for_windows(guild_id, current_stock, now, &DEFAULT_TREND_WINDOWS)
    }

    pub fn get_monetary_trends_for_windows(
        &self,
        guild_id: Option<i64>,
        current_stock: i64,
        now: i64,
        windows: &[i64],
    ) -> Result<MonetaryTrends, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut trends = BTreeMap::new();
        for requested_days in windows {
            let days = (*requested_days).max(1);
            let target_age = days * SECONDS_PER_DAY;
            let minimum_age = target_age * 3 / 4;
            let maximum_age = target_age * 2;
            let anchor = connection
                .query_row(
                    "SELECT snapshot_date, captured_at, monetary_stock
                     FROM economy_daily_snapshots
                     WHERE guild_id=?1 AND captured_at BETWEEN ?2 AND ?3
                     ORDER BY ABS(captured_at-?4), captured_at DESC LIMIT 1",
                    params![
                        guild_id,
                        now - maximum_age,
                        now - minimum_age,
                        now - target_age,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            let trend = anchor.and_then(|(anchor_date, captured_at, anchor_stock)| {
                let elapsed_days = (now - captured_at) as f64 / SECONDS_PER_DAY as f64;
                if anchor_stock <= 0 || current_stock <= 0 || elapsed_days <= 0.0 {
                    return None;
                }
                let ratio = current_stock as f64 / anchor_stock as f64;
                let change_jc = current_stock - anchor_stock;
                Some(MonetaryTrend {
                    requested_days: days,
                    elapsed_days,
                    anchor_date,
                    anchor_stock,
                    current_stock,
                    change_jc,
                    period_rate: ratio - 1.0,
                    daily_rate: ratio.powf(1.0 / elapsed_days) - 1.0,
                    annualized_rate: ratio.powf(365.0 / elapsed_days) - 1.0,
                    average_daily_change_jc: change_jc as f64 / elapsed_days,
                })
            });
            trends.insert(days, trend);
        }
        Ok(trends)
    }

    pub fn get_flow_indicators(
        &self,
        guild_id: Option<i64>,
        current_stock: i64,
        player_count: usize,
        now: i64,
    ) -> Result<FlowIndicators, EconomyEventRepositoryError> {
        self.get_flow_indicators_for_windows(
            guild_id,
            current_stock,
            player_count,
            now,
            &DEFAULT_FLOW_WINDOWS,
        )
    }

    pub fn get_flow_indicators_for_windows(
        &self,
        guild_id: Option<i64>,
        current_stock: i64,
        player_count: usize,
        now: i64,
        windows: &[i64],
    ) -> Result<FlowIndicators, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut indicators = BTreeMap::new();
        for requested_days in windows {
            let days = (*requested_days).max(1);
            let cutoff = now - days * SECONDS_PER_DAY;
            let raw = connection.query_row(
                "SELECT COALESCE(SUM(delta),0),
                        COALESCE(SUM(ABS(delta)),0),
                        COALESCE(SUM(CASE WHEN delta>0 THEN delta ELSE 0 END),0),
                        COALESCE(-SUM(CASE WHEN delta<0 THEN delta ELSE 0 END),0),
                        COUNT(*),
                        COUNT(DISTINCT CASE WHEN account_type='player'
                            THEN account_id END)
                 FROM economy_ledger_entries
                 WHERE guild_id=?1 AND created_at BETWEEN ?2 AND ?3
                   AND source!='ledger_backfill'",
                params![guild_id, cutoff, now],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )?;
            let entry_count = usize::try_from(raw.4).unwrap_or_default();
            let active_players = usize::try_from(raw.5).unwrap_or_default();
            indicators.insert(
                days,
                FlowIndicator {
                    days,
                    net_flow_jc: raw.0,
                    gross_flow_jc: raw.1,
                    inflow_jc: raw.2,
                    outflow_jc: raw.3,
                    entry_count,
                    active_players,
                    active_player_rate: if player_count == 0 {
                        0.0
                    } else {
                        active_players as f64 / player_count as f64
                    },
                    average_daily_net_jc: raw.0 as f64 / days as f64,
                    average_daily_gross_jc: raw.1 as f64 / days as f64,
                    daily_turnover_rate: if current_stock > 0 {
                        raw.1 as f64 / days as f64 / current_stock as f64
                    } else {
                        0.0
                    },
                },
            );
        }
        Ok(indicators)
    }

    pub fn get_distribution_indicators(
        &self,
        guild_id: Option<i64>,
    ) -> Result<DistributionIndicators, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT jopacoin_balance FROM players
             WHERE guild_id=?1 AND jopacoin_balance>0 ORDER BY jopacoin_balance",
        )?;
        let balances = statement
            .query_map([Self::normalize_guild_id(guild_id)], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        let count = balances.len();
        let total: i64 = balances.iter().sum();
        if count == 0 || total <= 0 {
            return Ok(DistributionIndicators::default());
        }
        let top_decile_count = count.div_ceil(10).max(1);
        let midpoint = count / 2;
        let median = if count % 2 == 1 {
            balances[midpoint] as f64
        } else {
            (balances[midpoint - 1] as f64 + balances[midpoint] as f64) / 2.0
        };
        let weighted_sum: i128 = balances
            .iter()
            .enumerate()
            .map(|(index, balance)| (index as i128 + 1) * i128::from(*balance))
            .sum();
        let gini = (2.0 * weighted_sum as f64) / (count as f64 * total as f64)
            - (count as f64 + 1.0) / count as f64;
        Ok(DistributionIndicators {
            positive_player_count: count,
            median_positive_wallet: median,
            top_decile_count,
            top_decile_share: balances[count - top_decile_count..].iter().sum::<i64>() as f64
                / total as f64,
            gini: gini.clamp(0.0, 1.0),
        })
    }

    pub fn get_surface_daily_volumes(
        &self,
        guild_id: Option<i64>,
        lookback_days: i64,
        now: i64,
    ) -> Result<SurfaceVolumes, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        let days = lookback_days.max(1);
        let cutoff = now - days * SECONDS_PER_DAY;
        let mut statement = connection.prepare(
            "SELECT source,
                    COALESCE(SUM(CASE WHEN delta>0 THEN delta ELSE 0 END),0),
                    COALESCE(-SUM(CASE WHEN delta<0 THEN delta ELSE 0 END),0)
             FROM economy_ledger_entries
             WHERE guild_id=?1 AND created_at>=?2
               AND source IN ('dig','trivia','player_trivia','mana_reward',
                   'manashop_buff','gamba','bet_settlement','prediction_resolution')
             GROUP BY source",
        )?;
        let rows = statement
            .query_map(params![Self::normalize_guild_id(guild_id), cutoff], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut result = SurfaceVolumes::default();
        for (source, credits, debits) in rows {
            match source.as_str() {
                "dig" | "trivia" | "player_trivia" | "mana_reward" | "manashop_buff" => {
                    result.reward_credits += credits as f64 / days as f64;
                }
                "gamba" => {
                    result.gamba_credits += credits as f64 / days as f64;
                    result.gamba_debits += debits as f64 / days as f64;
                }
                "bet_settlement" => result.bet_payouts += credits as f64 / days as f64,
                "prediction_resolution" => {
                    result.prediction_payouts += credits as f64 / days as f64;
                }
                _ => {}
            }
        }
        Ok(result)
    }

    pub fn get_event_for_date(
        &self,
        guild_id: Option<i64>,
        event_date: &str,
    ) -> Result<Option<EconomyEvent>, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        select_event_for_date(&connection, Self::normalize_guild_id(guild_id), event_date)
    }

    /// Insert a daily event and apply reserve/wallet effects exactly once.
    ///
    /// `BEGIN IMMEDIATE` serializes this operation with Python and other Rust
    /// writers before the unique `(guild_id, event_date)` check. The event row,
    /// ledger context, trigger-written entries, and final effect JSON commit or
    /// roll back as one unit.
    pub fn activate_event_atomic(
        &self,
        guild_id: Option<i64>,
        draft: &EventDraft,
    ) -> Result<ActivationResult, EconomyEventRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = select_event_for_date(&transaction, guild_id, &draft.event_date)? {
            transaction.commit()?;
            return Ok(ActivationResult {
                event: existing,
                created: false,
            });
        }

        let mut effects = draft.effects.clone();
        transaction.execute(
            "INSERT INTO economy_daily_events (
                 guild_id,event_date,name,hero,direction,severity,
                 target_effect_jc,forecast_flow_jc,expected_effect_jc,
                 direct_effect_jc,monetary_stock_before,effects,announcement,
                 starts_at,ends_at,created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,?11,?12,?13,?14,?15)",
            params![
                guild_id,
                draft.event_date,
                draft.name,
                draft.hero,
                draft.direction.as_str(),
                draft.severity,
                draft.target_effect_jc,
                draft.forecast_flow_jc,
                draft.expected_effect_jc,
                draft.monetary_stock_before,
                effects.to_json(),
                draft.announcement,
                draft.starts_at,
                draft.ends_at,
                draft.created_at,
            ],
        )?;
        let event_id = transaction.last_insert_rowid();
        let mut direct_effect_jc = 0_i64;
        let mut available = transaction
            .query_row(
                "SELECT COALESCE(total_collected,0) FROM nonprofit_fund WHERE guild_id=?1",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);

        let reserve_burn = available.min(effects.reserve_burn_jc.max(0));
        if reserve_burn > 0 {
            let reason = format!("{} reserve burn", draft.name);
            let metadata = event_date_metadata(&draft.event_date);
            with_ledger_context(&transaction, event_id, &reason, &metadata, || {
                transaction.execute(
                    "UPDATE nonprofit_fund
                         SET total_collected=total_collected-?1,
                             updated_at=CURRENT_TIMESTAMP
                         WHERE guild_id=?2",
                    params![reserve_burn, guild_id],
                )?;
                Ok(())
            })?;
            available -= reserve_burn;
            direct_effect_jc -= reserve_burn;
        }

        let wallet_burn_rate = effects.wallet_burn_rate.max(0.0);
        let mut wallet_burn_jc = 0_i64;
        if wallet_burn_rate > 0.0 {
            let players = player_balances(&transaction, guild_id)?;
            let reason = format!("{} wallet burn", draft.name);
            let metadata = format!(
                "{{\"event_date\": {}, \"rate\": {}}}",
                json_string(&draft.event_date),
                float_json(wallet_burn_rate),
            );
            with_ledger_context(&transaction, event_id, &reason, &metadata, || {
                for (player_id, balance) in &players {
                    let debit = (*balance as f64 * wallet_burn_rate) as i64;
                    if debit <= 0 {
                        continue;
                    }
                    transaction.execute(
                        "UPDATE players
                             SET jopacoin_balance=jopacoin_balance-?1,
                                 updated_at=CURRENT_TIMESTAMP
                             WHERE guild_id=?2 AND discord_id=?3",
                        params![debit, guild_id, player_id],
                    )?;
                    wallet_burn_jc += debit;
                }
                Ok(())
            })?;
            direct_effect_jc -= wallet_burn_jc;
        }

        let requested_release = effects.reserve_release_jc.max(0);
        let reserve_release = available.min(requested_release);
        let mut released_jc = 0_i64;
        if reserve_release > 0 {
            let player_ids = player_ids(&transaction, guild_id)?;
            if !player_ids.is_empty() {
                let player_count = i64::try_from(player_ids.len()).unwrap_or(i64::MAX);
                let quotient = reserve_release / player_count;
                let remainder = reserve_release % player_count;
                let reason = format!("{} reserve release", draft.name);
                let metadata = event_date_metadata(&draft.event_date);
                with_ledger_context(&transaction, event_id, &reason, &metadata, || {
                    transaction.execute(
                        "UPDATE nonprofit_fund
                             SET total_collected=total_collected-?1,
                                 updated_at=CURRENT_TIMESTAMP
                             WHERE guild_id=?2",
                        params![reserve_release, guild_id],
                    )?;
                    for (index, player_id) in player_ids.iter().enumerate() {
                        let credit = quotient
                            + i64::from(i64::try_from(index).unwrap_or(i64::MAX) < remainder);
                        if credit <= 0 {
                            continue;
                        }
                        transaction.execute(
                            "UPDATE players
                                 SET jopacoin_balance=jopacoin_balance+?1,
                                     updated_at=CURRENT_TIMESTAMP
                                 WHERE guild_id=?2 AND discord_id=?3",
                            params![credit, guild_id, player_id],
                        )?;
                        released_jc += credit;
                    }
                    Ok(())
                })?;
            }
        }

        effects.reserve_burn_jc = reserve_burn;
        effects.wallet_burn_jc = wallet_burn_jc;
        effects.reserve_release_jc = released_jc;
        transaction.execute(
            "UPDATE economy_daily_events
             SET effects=?1,direct_effect_jc=?2 WHERE event_id=?3",
            params![effects.to_json(), direct_effect_jc, event_id],
        )?;
        let event = select_event_by_id(&transaction, event_id)?.ok_or_else(|| {
            EconomyEventRepositoryError::InvalidDatabaseValue {
                field: "economy_daily_events.event_id",
                value: event_id.to_string(),
            }
        })?;
        transaction.commit()?;
        Ok(ActivationResult {
            event,
            created: true,
        })
    }

    /// Stamp only the first successful initial announcement attempt.
    pub fn mark_event_announced(
        &self,
        guild_id: Option<i64>,
        event_id: i64,
        now: i64,
    ) -> Result<bool, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE economy_daily_events SET announced_at=?1
             WHERE guild_id=?2 AND event_id=?3 AND announced_at IS NULL",
            params![now, Self::normalize_guild_id(guild_id), event_id],
        )? == 1)
    }

    /// Stamp only the first successful twelve-hour reminder attempt.
    pub fn mark_event_reminder_announced(
        &self,
        guild_id: Option<i64>,
        event_id: i64,
        now: i64,
    ) -> Result<bool, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE economy_daily_events SET reminder_announced_at=?1
             WHERE guild_id=?2 AND event_id=?3 AND reminder_announced_at IS NULL",
            params![now, Self::normalize_guild_id(guild_id), event_id],
        )? == 1)
    }

    /// Reconcile the nearest earlier event only once.
    pub fn reconcile_prior_event(
        &self,
        guild_id: Option<i64>,
        event_date: &str,
        current_stock: i64,
    ) -> Result<bool, EconomyEventRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE economy_daily_events
             SET actual_stock_change_jc=?1-monetary_stock_before
             WHERE event_id=(
                 SELECT event_id FROM economy_daily_events
                 WHERE guild_id=?2 AND event_date<?3
                 ORDER BY event_date DESC LIMIT 1
             ) AND actual_stock_change_jc IS NULL",
            params![
                current_stock,
                Self::normalize_guild_id(guild_id),
                event_date,
            ],
        )? == 1)
    }
}

fn annualized_change(
    connection: &Connection,
    guild_id: i64,
    snapshot_date: &str,
    current_stock: i64,
    days: i64,
) -> Result<Option<f64>, rusqlite::Error> {
    let anchor = connection
        .query_row(
            "SELECT monetary_stock, julianday(?1)-julianday(snapshot_date)
             FROM economy_daily_snapshots
             WHERE guild_id=?2 AND snapshot_date<=date(?1,?3)
             ORDER BY snapshot_date DESC LIMIT 1",
            params![snapshot_date, guild_id, format!("-{days} days")],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                ))
            },
        )
        .optional()?;
    Ok(anchor.and_then(|(old_stock, elapsed)| {
        if old_stock <= 0 || current_stock <= 0 || elapsed < (days as f64 * 0.8).max(1.0) {
            None
        } else {
            Some((current_stock as f64 / old_stock as f64).powf(365.0 / elapsed) - 1.0)
        }
    }))
}

fn row_to_snapshot(row: &Row<'_>) -> rusqlite::Result<Snapshot> {
    let player_count = usize::try_from(row.get::<_, i64>(6)?).unwrap_or_default();
    Ok(Snapshot {
        guild_id: row.get(0)?,
        snapshot_date: row.get(1)?,
        captured_at: row.get(2)?,
        balance_sheet: BalanceSheet {
            player_wallets: row.get(3)?,
            positive_wallets: row.get(4)?,
            visible_debt: row.get(5)?,
            player_count,
            average_wallet: row.get(7)?,
            reserve_available: row.get(8)?,
            reserve_locked: row.get(9)?,
            reserve_next_match_pot: row.get(10)?,
            prediction_open_cash: row.get(11)?,
            wager_escrow: row.get(12)?,
            monetary_stock: row.get(13)?,
        },
        annualized_30d: row.get(14)?,
        annualized_90d: row.get(15)?,
    })
}

const EVENT_SELECT: &str = "SELECT event_id,guild_id,event_date,name,hero,direction,severity,
            target_effect_jc,forecast_flow_jc,expected_effect_jc,direct_effect_jc,
            actual_stock_change_jc,monetary_stock_before,effects,announcement,
            starts_at,ends_at,created_at,announced_at,reminder_announced_at,
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.reward_multiplier') END,1.0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.gamba_win_multiplier') END,1.0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.gamba_loss_multiplier') END,1.0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.bet_payout_multiplier') END,1.0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.prediction_payout_multiplier') END,1.0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.prediction_spread_ticks_delta') END,0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.reserve_burn_jc') END,0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.reserve_release_jc') END,0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.wallet_burn_rate') END,0.0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.wallet_burn_jc') END,0),
            COALESCE(CASE WHEN json_valid(effects)
                THEN json_extract(effects,'$.reserve_voting_disabled') END,0)
            ,CASE WHEN json_valid(effects)
                       AND json_type(effects,'$.reserve_voting_disabled') IS NOT NULL
                  THEN 1 ELSE 0 END
     FROM economy_daily_events";

fn select_event_for_date(
    connection: &Connection,
    guild_id: i64,
    event_date: &str,
) -> Result<Option<EconomyEvent>, EconomyEventRepositoryError> {
    let query = format!("{EVENT_SELECT} WHERE guild_id=?1 AND event_date=?2");
    let raw = connection
        .query_row(&query, params![guild_id, event_date], raw_event_row)
        .optional()?;
    raw.map_or(Ok(None), |raw| raw.into_event().map(Some))
}

fn select_event_by_id(
    connection: &Connection,
    event_id: i64,
) -> Result<Option<EconomyEvent>, EconomyEventRepositoryError> {
    let query = format!("{EVENT_SELECT} WHERE event_id=?1");
    let raw = connection
        .query_row(&query, [event_id], raw_event_row)
        .optional()?;
    raw.map_or(Ok(None), |raw| raw.into_event().map(Some))
}

struct RawEconomyEvent {
    event_id: i64,
    guild_id: i64,
    event_date: String,
    name: String,
    hero: String,
    direction: String,
    severity: i64,
    target_effect_jc: i64,
    forecast_flow_jc: i64,
    expected_effect_jc: i64,
    direct_effect_jc: i64,
    actual_stock_change_jc: Option<i64>,
    monetary_stock_before: i64,
    effects_json: String,
    announcement: String,
    starts_at: i64,
    ends_at: i64,
    created_at: i64,
    announced_at: Option<i64>,
    reminder_announced_at: Option<i64>,
    effects: EventEffects,
    reserve_voting_disabled_explicit: bool,
}

impl RawEconomyEvent {
    fn into_event(self) -> Result<EconomyEvent, EconomyEventRepositoryError> {
        Ok(EconomyEvent {
            event_id: self.event_id,
            guild_id: self.guild_id,
            event_date: self.event_date,
            name: self.name,
            hero: self.hero,
            direction: EventDirection::from_database(&self.direction)?,
            severity: self.severity,
            target_effect_jc: self.target_effect_jc,
            forecast_flow_jc: self.forecast_flow_jc,
            expected_effect_jc: self.expected_effect_jc,
            direct_effect_jc: self.direct_effect_jc,
            actual_stock_change_jc: self.actual_stock_change_jc,
            monetary_stock_before: self.monetary_stock_before,
            effects_json: self.effects_json,
            effects: self.effects,
            reserve_voting_disabled_explicit: self.reserve_voting_disabled_explicit,
            announcement: self.announcement,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            created_at: self.created_at,
            announced_at: self.announced_at,
            reminder_announced_at: self.reminder_announced_at,
        })
    }
}

fn raw_event_row(row: &Row<'_>) -> rusqlite::Result<RawEconomyEvent> {
    Ok(RawEconomyEvent {
        event_id: row.get(0)?,
        guild_id: row.get(1)?,
        event_date: row.get(2)?,
        name: row.get(3)?,
        hero: row.get(4)?,
        direction: row.get(5)?,
        severity: row.get(6)?,
        target_effect_jc: row.get(7)?,
        forecast_flow_jc: row.get(8)?,
        expected_effect_jc: row.get(9)?,
        direct_effect_jc: row.get(10)?,
        actual_stock_change_jc: row.get(11)?,
        monetary_stock_before: row.get(12)?,
        effects_json: row.get(13)?,
        announcement: row.get(14)?,
        starts_at: row.get(15)?,
        ends_at: row.get(16)?,
        created_at: row.get(17)?,
        announced_at: row.get(18)?,
        reminder_announced_at: row.get(19)?,
        effects: EventEffects {
            reward_multiplier: row.get(20)?,
            gamba_win_multiplier: row.get(21)?,
            gamba_loss_multiplier: row.get(22)?,
            bet_payout_multiplier: row.get(23)?,
            prediction_payout_multiplier: row.get(24)?,
            // Retired legacy setting: public behavior is always neutral.
            prediction_depth_multiplier: 1.0,
            prediction_spread_ticks_delta: row.get(25)?,
            reserve_burn_jc: row.get(26)?,
            reserve_release_jc: row.get(27)?,
            wallet_burn_rate: row.get(28)?,
            wallet_burn_jc: row.get(29)?,
            reserve_voting_disabled: row.get::<_, i64>(30)? != 0,
        },
        reserve_voting_disabled_explicit: row.get::<_, i64>(31)? != 0,
    })
}

fn player_balances(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<Vec<(i64, i64)>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT discord_id,jopacoin_balance FROM players
         WHERE guild_id=?1 AND jopacoin_balance>0 ORDER BY discord_id",
    )?;
    statement
        .query_map([guild_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn player_ids(transaction: &Transaction<'_>, guild_id: i64) -> Result<Vec<i64>, rusqlite::Error> {
    let mut statement = transaction
        .prepare("SELECT discord_id FROM players WHERE guild_id=?1 ORDER BY discord_id")?;
    statement.query_map([guild_id], |row| row.get(0))?.collect()
}

fn with_ledger_context<F>(
    transaction: &Transaction<'_>,
    event_id: i64,
    reason: &str,
    metadata: &str,
    operation: F,
) -> Result<(), rusqlite::Error>
where
    F: FnOnce() -> Result<(), rusqlite::Error>,
{
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id,source,related_type,related_id,reason,metadata
         ) VALUES (1,'economy_event','daily_economy_event',?1,?2,?3)",
        params![event_id.to_string(), reason, metadata],
    )?;
    let result = operation();
    let clear_result = transaction.execute("DELETE FROM economy_ledger_context", []);
    result?;
    clear_result?;
    Ok(())
}

fn event_date_metadata(event_date: &str) -> String {
    format!("{{\"event_date\": {}}}", json_string(event_date))
}

fn float_json(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        format!("{value:?}")
    }
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(character)).expect("write to String");
            }
            character => output.push(character),
        }
    }
    output.push('\"');
    output
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;

    use rusqlite::Connection;
    use tempfile::{NamedTempFile, tempdir};

    use super::*;
    use crate::test_support::FastTestDatabase;

    const GUILD: i64 = 987_654_321;

    fn fixture_template() -> &'static NamedTempFile {
        static TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();
        TEMPLATE.get_or_init(|| {
            let file = NamedTempFile::new().unwrap();
            Connection::open(file.path())
                .unwrap()
                .execute_batch(EXACT_SCHEMA)
                .unwrap();
            file
        })
    }

    fn fixture() -> FastTestDatabase {
        FastTestDatabase::from_template(fixture_template().path())
    }

    fn durable_fixture() -> NamedTempFile {
        let file = NamedTempFile::new().unwrap();
        std::fs::copy(fixture_template().path(), file.path()).unwrap();
        file
    }

    fn seed_economy(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "INSERT INTO players (discord_id,guild_id,name,jopacoin_balance)
                 VALUES (1,?1,'one',1000),(2,?1,'two',500)",
                [GUILD],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO nonprofit_fund (guild_id,total_collected) VALUES (?1,1000)",
                [GUILD],
            )
            .unwrap();
    }

    fn sheet(stock: i64) -> BalanceSheet {
        BalanceSheet {
            monetary_stock: stock,
            ..BalanceSheet::default()
        }
    }

    fn draft(event_date: &str) -> EventDraft {
        EventDraft {
            event_date: event_date.to_owned(),
            name: "Ravage".to_owned(),
            hero: "Tidehunter".to_owned(),
            direction: EventDirection::Deflationary,
            severity: 2,
            target_effect_jc: -100,
            forecast_flow_jc: 90,
            expected_effect_jc: -110,
            monetary_stock_before: 2_500,
            effects: EventEffects {
                reward_multiplier: 0.8,
                gamba_win_multiplier: 0.9,
                gamba_loss_multiplier: 1.1,
                bet_payout_multiplier: 0.97,
                prediction_payout_multiplier: 0.99,
                prediction_depth_multiplier: 0.7,
                prediction_spread_ticks_delta: 2,
                reserve_burn_jc: 100,
                reserve_release_jc: 0,
                wallet_burn_rate: 0.01,
                wallet_burn_jc: 0,
                reserve_voting_disabled: false,
            },
            announcement: "A tidal shock hits the economy.".to_owned(),
            starts_at: 1_000,
            ends_at: 87_400,
            created_at: 900,
        }
    }

    #[test]
    fn production_repository_does_not_create_or_migrate_missing_database() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("missing.sqlite");
        let repository = EconomyEventRepository::new(&path);

        assert!(repository.get_policy_state(Some(GUILD)).is_err());
        assert!(!path.exists());
    }

    /// Run manually against a freshly copied, Python-upgraded development DB:
    /// `CAMA_ECONOMY_EVENT_DB_COPY=/path/to/copy cargo test -p cama-db
    /// economy_event_repository::tests::upgraded_dev_clone_repository_smoke
    /// -- --ignored --exact`.
    #[test]
    #[ignore = "requires an explicitly disposable, Python-upgraded database copy"]
    fn upgraded_dev_clone_repository_smoke() {
        const SENTINEL_GUILD: i64 = 9_223_372_036_854_000_001;
        let path = std::env::var("CAMA_ECONOMY_EVENT_DB_COPY")
            .expect("CAMA_ECONOMY_EVENT_DB_COPY must name a disposable copy");
        let repository = EconomyEventRepository::new(path);

        let balance = repository
            .capture_balance_sheet(Some(SENTINEL_GUILD))
            .unwrap();
        assert_eq!(balance, BalanceSheet::default());
        let policy = repository
            .ensure_policy_state(
                Some(SENTINEL_GUILD),
                PolicyMode::Normal,
                0.02,
                0.02,
                4_102_444_800,
            )
            .unwrap();
        assert_eq!(policy.mode, PolicyMode::Normal);
        repository
            .save_snapshot(Some(SENTINEL_GUILD), "9999-12-31", &balance, 4_102_444_800)
            .unwrap();
        let mut event = draft("9999-12-31");
        event.direction = EventDirection::Neutral;
        event.effects = EventEffects::default();
        event.monetary_stock_before = 0;
        let activated = repository
            .activate_event_atomic(Some(SENTINEL_GUILD), &event)
            .unwrap();
        let retried = repository
            .activate_event_atomic(Some(SENTINEL_GUILD), &event)
            .unwrap();
        assert!(activated.created);
        assert!(!retried.created);
        assert_eq!(activated.event.event_id, retried.event.event_id);
        assert!(
            repository
                .mark_event_announced(
                    Some(SENTINEL_GUILD),
                    activated.event.event_id,
                    4_102_444_801,
                )
                .unwrap()
        );
    }

    #[test]
    fn policy_recovery_start_is_stable_and_guild_scoped() {
        let file = fixture();
        let repository = EconomyEventRepository::new(file.path());
        let first = repository
            .ensure_policy_state(Some(GUILD), PolicyMode::Recovery, 0.02, 0.04, 100)
            .unwrap();
        let retry = repository
            .ensure_policy_state(Some(GUILD), PolicyMode::Recovery, 0.03, 0.05, 200)
            .unwrap();
        repository
            .ensure_policy_state(Some(GUILD + 1), PolicyMode::Disabled, 0.0, 0.0, 300)
            .unwrap();

        assert_eq!(first.recovery_started_at, Some(100));
        assert_eq!(retry.recovery_started_at, Some(100));
        assert_eq!(retry.target_annual_rate, 0.03);
        assert_eq!(
            repository
                .get_policy_state(Some(GUILD + 1))
                .unwrap()
                .unwrap()
                .mode,
            PolicyMode::Disabled
        );
    }

    #[test]
    fn test_policy_targets_two_percent_without_a_recovery_override() {
        let file = fixture();
        let repository = EconomyEventRepository::new(file.path());
        let policy = repository
            .ensure_policy_state(Some(GUILD), PolicyMode::Normal, 0.02, 0.02, 1_000)
            .unwrap();

        assert_eq!(policy.mode, PolicyMode::Normal);
        assert!((policy.target_annual_rate - 0.02).abs() < f64::EPSILON);
        assert_eq!(policy.recovery_started_at, None);
    }

    #[test]
    fn test_balance_sheet_counts_reserve_and_average() {
        let file = fixture();
        seed_economy(file.path());
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute(
                "INSERT INTO players (discord_id,guild_id,name,jopacoin_balance)
                 VALUES (3,?1,'debt',-25)",
                [GUILD],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO players (discord_id,guild_id,name,jopacoin_balance)
                 VALUES (9,?1,'other',9999)",
                [GUILD + 1],
            )
            .unwrap();
        let repository = EconomyEventRepository::new(file.path());
        let balance = repository.capture_balance_sheet(Some(GUILD)).unwrap();

        assert_eq!(balance.player_wallets, 1_475);
        assert_eq!(balance.positive_wallets, 1_500);
        assert_eq!(balance.visible_debt, 25);
        assert_eq!(balance.player_count, 3);
        assert!((balance.average_wallet - 1_475.0 / 3.0).abs() < f64::EPSILON);
        assert_eq!(balance.reserve_available, 1_000);
        assert_eq!(balance.monetary_stock, 2_475);
    }

    #[test]
    fn balance_sheet_includes_locked_prediction_and_wager_surfaces() {
        let file = fixture();
        seed_economy(file.path());
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(&format!(
                "INSERT INTO disburse_proposals VALUES ({GUILD},70,'active');
                 INSERT INTO pending_matches VALUES ({GUILD},11);
                 INSERT INTO first_game_pool_claims VALUES ({GUILD},11,20,0);
                 INSERT INTO predictions VALUES ({GUILD},'open',30);
                 INSERT INTO duel_challenges VALUES ({GUILD},'accepted',40);
                 INSERT INTO mafia_games VALUES ({GUILD},'ACTIVE','RUNNING',5,4);
                 INSERT INTO bets VALUES ({GUILD},11,10,3);
                 UPDATE nonprofit_fund SET first_game_open_pool=6,
                     first_game_lowskill_pool=4,next_match_pot=50 WHERE guild_id={GUILD};"
            ))
            .unwrap();
        let repository = EconomyEventRepository::new(file.path());
        let balance = repository.capture_balance_sheet(Some(GUILD)).unwrap();

        assert_eq!(balance.reserve_locked, 100);
        assert_eq!(balance.reserve_next_match_pot, 50);
        assert_eq!(balance.prediction_open_cash, 30);
        assert_eq!(balance.wager_escrow, 130);
        assert_eq!(balance.monetary_stock, 2_810);
    }

    #[test]
    fn test_reward_volume_includes_trivia_and_generated_mana() {
        let file = fixture();
        let repository = EconomyEventRepository::new(file.path());
        let now = 2_000_000;
        let connection = Connection::open(file.path()).unwrap();
        for (source, amount) in [
            ("dig", 10),
            ("trivia", 20),
            ("player_trivia", 30),
            ("mana_reward", 40),
            ("manashop_buff", 50),
        ] {
            connection
                .execute(
                    "INSERT INTO economy_ledger_entries (
                         guild_id,account_type,account_id,delta,balance_before,
                         balance_after,source,created_at
                     ) VALUES (?1,'player',1,?2,0,?2,?3,?4)",
                    params![GUILD, amount, source, now - 1],
                )
                .unwrap();
        }

        let volumes = repository
            .get_surface_daily_volumes(Some(GUILD), 1, now)
            .unwrap();
        assert_eq!(volumes.reward_credits, 150.0);
    }

    #[test]
    fn test_live_monetary_trends_use_nearest_daily_snapshot_anchors() {
        let file = fixture();
        let repository = EconomyEventRepository::new(file.path());
        let now = 2_000_000_000;
        for (days_ago, stock) in [(1, 1_050), (3, 1_000), (7, 900)] {
            repository
                .save_snapshot(
                    Some(GUILD),
                    &format!("2026-07-{:02}", 18 - days_ago),
                    &sheet(stock),
                    now - days_ago * SECONDS_PER_DAY,
                )
                .unwrap();
        }
        let trends = repository
            .get_monetary_trends(Some(GUILD), 1_100, now)
            .unwrap();

        assert_eq!(trends[&1].as_ref().unwrap().change_jc, 50);
        assert!(
            (trends[&1].as_ref().unwrap().period_rate - (1_100.0 / 1_050.0 - 1.0)).abs() < 1e-12
        );
        assert_eq!(trends[&3].as_ref().unwrap().elapsed_days, 3.0);
        assert!((trends[&3].as_ref().unwrap().average_daily_change_jc - 100.0 / 3.0).abs() < 1e-12);
        assert_eq!(trends[&7].as_ref().unwrap().change_jc, 200);
    }

    #[test]
    fn test_monetary_trends_report_missing_windows_without_fake_rates() {
        let file = fixture();
        let repository = EconomyEventRepository::new(file.path());
        let trends = repository
            .get_monetary_trends(Some(GUILD), 1_000, 2_000_000_000)
            .unwrap();

        assert_eq!(trends, BTreeMap::from([(1, None), (3, None), (7, None)]));
    }

    #[test]
    fn snapshot_annualization_and_upsert_match_python() {
        let file = fixture();
        let repository = EconomyEventRepository::new(file.path());
        repository
            .save_snapshot(Some(GUILD), "2026-06-01", &sheet(1_000), 1)
            .unwrap();
        let saved = repository
            .save_snapshot(Some(GUILD), "2026-07-01", &sheet(1_100), 2)
            .unwrap();
        repository
            .save_snapshot(Some(GUILD), "2026-07-01", &sheet(1_200), 3)
            .unwrap();

        assert!(saved.annualized_30d.unwrap() > 2.0);
        let latest = repository
            .get_latest_snapshot(Some(GUILD))
            .unwrap()
            .unwrap();
        assert_eq!(latest.captured_at, 3);
        assert_eq!(latest.balance_sheet.monetary_stock, 1_200);
    }

    #[test]
    fn test_flow_indicators_measure_rolling_turnover_and_participation() {
        let file = fixture();
        let repository = EconomyEventRepository::new(file.path());
        let now = 2_000_000;
        let connection = Connection::open(file.path()).unwrap();
        for (account_type, account_id, delta, source, created_at) in [
            ("player", 1, 100, "dig", now - SECONDS_PER_DAY),
            ("player", 2, -40, "gamba", now - 2 * SECONDS_PER_DAY),
            (
                "nonprofit",
                GUILD,
                60,
                "tax_fine",
                now - 5 * SECONDS_PER_DAY,
            ),
            ("player", 3, 999, "ledger_backfill", now - SECONDS_PER_DAY),
        ] {
            connection
                .execute(
                    "INSERT INTO economy_ledger_entries (
                         guild_id,account_type,account_id,delta,balance_before,
                         balance_after,source,created_at
                     ) VALUES (?1,?2,?3,?4,0,?4,?5,?6)",
                    params![GUILD, account_type, account_id, delta, source, created_at],
                )
                .unwrap();
        }

        let flows = repository
            .get_flow_indicators(Some(GUILD), 1_000, 4, now)
            .unwrap();
        assert_eq!(flows[&3].net_flow_jc, 60);
        assert_eq!(flows[&3].gross_flow_jc, 140);
        assert_eq!(flows[&3].active_players, 2);
        assert_eq!(flows[&3].active_player_rate, 0.5);
        assert!((flows[&3].daily_turnover_rate - 140.0 / 3.0 / 1_000.0).abs() < 1e-12);
        assert_eq!(flows[&7].net_flow_jc, 120);
        assert_eq!(flows[&7].gross_flow_jc, 200);
    }

    #[test]
    fn test_distribution_indicators_report_concentration() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        for (index, balance) in [10, 20, 30, 40, 100].into_iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO players (discord_id,guild_id,name,jopacoin_balance)
                     VALUES (?1,?2,?3,?4)",
                    params![
                        i64::try_from(index).unwrap() + 1,
                        GUILD,
                        format!("player-{index}"),
                        balance,
                    ],
                )
                .unwrap();
        }
        let repository = EconomyEventRepository::new(file.path());
        let distribution = repository.get_distribution_indicators(Some(GUILD)).unwrap();

        assert_eq!(distribution.positive_player_count, 5);
        assert_eq!(distribution.median_positive_wallet, 30.0);
        assert_eq!(distribution.top_decile_count, 1);
        assert_eq!(distribution.top_decile_share, 0.5);
        assert!((distribution.gini - 0.4).abs() < 1e-12);
    }

    #[test]
    fn test_atomic_event_burns_once_and_records_ledger() {
        let file = fixture();
        seed_economy(file.path());
        let repository = EconomyEventRepository::new(file.path());
        let first = repository
            .activate_event_atomic(Some(GUILD), &draft("2026-07-18"))
            .unwrap();
        let second = repository
            .activate_event_atomic(Some(GUILD), &draft("2026-07-18"))
            .unwrap();

        assert!(first.created);
        assert!(!second.created);
        assert_eq!(second.event.event_id, first.event.event_id);
        assert_eq!(first.event.effects.reserve_burn_jc, 100);
        assert_eq!(first.event.effects.wallet_burn_jc, 15);
        assert_eq!(first.event.direct_effect_jc, -115);
        let connection = Connection::open(file.path()).unwrap();
        let balances = connection
            .prepare("SELECT jopacoin_balance FROM players WHERE guild_id=?1 ORDER BY discord_id")
            .unwrap()
            .query_map([GUILD], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<i64>, _>>()
            .unwrap();
        assert_eq!(balances, vec![990, 495]);
        assert_eq!(
            connection
                .query_row(
                    "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
                    [GUILD],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            900
        );
        let ledger = connection
            .prepare(
                "SELECT account_type,SUM(delta),MIN(related_type),MIN(related_id)
                 FROM economy_ledger_entries
                 WHERE guild_id=?1 AND source='economy_event'
                 GROUP BY account_type ORDER BY account_type",
            )
            .unwrap()
            .query_map([GUILD], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            ledger,
            vec![
                (
                    "nonprofit".to_owned(),
                    -100,
                    "daily_economy_event".to_owned(),
                    first.event.event_id.to_string(),
                ),
                (
                    "player".to_owned(),
                    -15,
                    "daily_economy_event".to_owned(),
                    first.event.event_id.to_string(),
                ),
            ]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn concurrent_activation_has_one_winner_and_one_set_of_direct_legs() {
        let file = durable_fixture();
        seed_economy(file.path());
        let path = file.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    EconomyEventRepository::new(path)
                        .activate_event_atomic(Some(GUILD), &draft("2026-07-19"))
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(outcomes.iter().filter(|outcome| outcome.created).count(), 1);
        assert_eq!(outcomes[0].event.event_id, outcomes[1].event.event_id);
        let connection = Connection::open(file.path()).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE source='economy_event'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
    }

    #[test]
    fn failed_direct_leg_rolls_back_event_balances_and_ledger_context() {
        let file = fixture();
        seed_economy(file.path());
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_economy_event_burn
                 BEFORE UPDATE OF total_collected ON nonprofit_fund
                 WHEN (SELECT source FROM economy_ledger_context WHERE id=1)
                      ='economy_event'
                 BEGIN
                     SELECT RAISE(ABORT,'injected burn failure');
                 END;",
            )
            .unwrap();
        drop(connection);
        let repository = EconomyEventRepository::new(file.path());

        assert!(
            repository
                .activate_event_atomic(Some(GUILD), &draft("2026-07-19"))
                .is_err()
        );
        let connection = Connection::open(file.path()).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_daily_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT total_collected FROM nonprofit_fund", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1_000
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE source='economy_event'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn event_identity_and_direct_legs_are_guild_scoped() {
        let file = fixture();
        seed_economy(file.path());
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute(
                "INSERT INTO players (discord_id,guild_id,name,jopacoin_balance)
                 VALUES (1,?1,'other',200)",
                [GUILD + 1],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO nonprofit_fund (guild_id,total_collected) VALUES (?1,300)",
                [GUILD + 1],
            )
            .unwrap();
        let repository = EconomyEventRepository::new(file.path());

        let first = repository
            .activate_event_atomic(Some(GUILD), &draft("2026-07-19"))
            .unwrap();
        let other = repository
            .activate_event_atomic(Some(GUILD + 1), &draft("2026-07-19"))
            .unwrap();

        assert!(first.created);
        assert!(other.created);
        assert_ne!(first.event.event_id, other.event.event_id);
        let connection = Connection::open(file.path()).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE guild_id=?1 AND discord_id=1",
                    [GUILD + 1],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            198
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
                    [GUILD + 1],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            200
        );
    }

    #[test]
    fn test_atomic_event_release_credits_players_without_changing_supply() {
        let file = fixture();
        seed_economy(file.path());
        let repository = EconomyEventRepository::new(file.path());
        let mut release = draft("2026-07-20");
        release.effects.reserve_burn_jc = 0;
        release.effects.wallet_burn_rate = 0.0;
        release.effects.reserve_release_jc = 201;
        let result = repository
            .activate_event_atomic(Some(GUILD), &release)
            .unwrap();

        assert!(result.created);
        assert_eq!(result.event.effects.reserve_release_jc, 201);
        assert_eq!(result.event.direct_effect_jc, 0);
        let connection = Connection::open(file.path()).unwrap();
        let balances = connection
            .prepare("SELECT jopacoin_balance FROM players WHERE guild_id=?1 ORDER BY discord_id")
            .unwrap()
            .query_map([GUILD], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<i64>, _>>()
            .unwrap();
        assert_eq!(balances, vec![1_101, 600]);
        assert_eq!(
            connection
                .query_row("SELECT total_collected FROM nonprofit_fund", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            799
        );
        let ledger_total = connection
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                 WHERE source='economy_event'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(ledger_total, 0);
    }

    #[test]
    fn release_without_players_keeps_reserve_and_records_zero_release() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute(
                "INSERT INTO nonprofit_fund (guild_id,total_collected) VALUES (?1,1000)",
                [GUILD],
            )
            .unwrap();
        let repository = EconomyEventRepository::new(file.path());
        let mut release = draft("2026-07-20");
        release.effects.reserve_burn_jc = 0;
        release.effects.wallet_burn_rate = 0.0;
        release.effects.reserve_release_jc = 200;
        let result = repository
            .activate_event_atomic(Some(GUILD), &release)
            .unwrap();

        assert_eq!(result.event.effects.reserve_release_jc, 0);
        assert_eq!(
            Connection::open(file.path())
                .unwrap()
                .query_row("SELECT total_collected FROM nonprofit_fund", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1_000
        );
    }

    #[test]
    fn test_mark_event_announced_stamps_once() {
        let file = fixture();
        seed_economy(file.path());
        let repository = EconomyEventRepository::new(file.path());
        let event = repository
            .activate_event_atomic(Some(GUILD), &draft("2026-07-21"))
            .unwrap()
            .event;

        assert!(
            repository
                .mark_event_announced(Some(GUILD), event.event_id, 1_111)
                .unwrap()
        );
        assert!(
            !repository
                .mark_event_announced(Some(GUILD), event.event_id, 2_222)
                .unwrap()
        );
        assert_eq!(
            repository
                .get_event_for_date(Some(GUILD), "2026-07-21")
                .unwrap()
                .unwrap()
                .announced_at,
            Some(1_111)
        );
        assert!(
            !repository
                .mark_event_announced(Some(GUILD + 1), event.event_id, 3_333)
                .unwrap()
        );
    }

    #[test]
    fn test_event_has_one_initial_and_one_twelve_hour_reminder_slot() {
        let file = fixture();
        seed_economy(file.path());
        let repository = EconomyEventRepository::new(file.path());
        let event = repository
            .activate_event_atomic(Some(GUILD), &draft("2026-07-22"))
            .unwrap()
            .event;

        assert_eq!(event.announced_at, None);
        assert_eq!(event.reminder_announced_at, None);
        assert!(
            repository
                .mark_event_announced(Some(GUILD), event.event_id, 1_001)
                .unwrap()
        );
        assert!(
            repository
                .mark_event_reminder_announced(Some(GUILD), event.event_id, 44_201)
                .unwrap()
        );
        assert!(
            !repository
                .mark_event_reminder_announced(Some(GUILD), event.event_id, 44_202)
                .unwrap()
        );
        let stored = repository
            .get_event_for_date(Some(GUILD), "2026-07-22")
            .unwrap()
            .unwrap();
        assert_eq!(stored.announced_at, Some(1_001));
        assert_eq!(stored.reminder_announced_at, Some(44_201));
    }

    #[test]
    fn event_json_is_typed_and_legacy_prediction_depth_is_neutralized() {
        let file = fixture();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute(
                "INSERT INTO economy_daily_events (
                     guild_id,event_date,name,hero,direction,severity,
                     target_effect_jc,forecast_flow_jc,expected_effect_jc,
                     monetary_stock_before,effects,announcement,starts_at,ends_at,created_at
                 ) VALUES (?1,'2026-07-23','Legacy','Enigma','neutral',1,
                     0,0,0,1000,
                     '{\"reward_multiplier\":0.8,\"prediction_depth_multiplier\":0.2}',
                     'legacy',1,2,1)",
                [GUILD],
            )
            .unwrap();
        let repository = EconomyEventRepository::new(file.path());
        let event = repository
            .get_event_for_date(Some(GUILD), "2026-07-23")
            .unwrap()
            .unwrap();

        assert_eq!(event.effects.reward_multiplier, 0.8);
        assert_eq!(event.effects.prediction_depth_multiplier, 1.0);
        assert_eq!(event.effects.wallet_burn_jc, 0);
        assert!(!event.reserve_voting_disabled_explicit);
    }

    #[test]
    fn reconcile_prior_event_targets_nearest_prior_once_and_is_guild_scoped() {
        let file = fixture();
        seed_economy(file.path());
        let repository = EconomyEventRepository::new(file.path());
        repository
            .activate_event_atomic(Some(GUILD), &draft("2026-07-24"))
            .unwrap();
        let mut newer = draft("2026-07-25");
        newer.monetary_stock_before = 3_000;
        repository
            .activate_event_atomic(Some(GUILD), &newer)
            .unwrap();

        assert!(
            repository
                .reconcile_prior_event(Some(GUILD), "2026-07-26", 3_100)
                .unwrap()
        );
        assert!(
            !repository
                .reconcile_prior_event(Some(GUILD), "2026-07-26", 9_999)
                .unwrap()
        );
        let event = repository
            .get_event_for_date(Some(GUILD), "2026-07-25")
            .unwrap()
            .unwrap();
        assert_eq!(event.actual_stock_change_jc, Some(100));
        assert_eq!(
            repository
                .get_event_for_date(Some(GUILD), "2026-07-24")
                .unwrap()
                .unwrap()
                .actual_stock_change_jc,
            None
        );
    }

    const EXACT_SCHEMA: &str = r#"
        CREATE TABLE players (
            discord_id INTEGER NOT NULL,
            guild_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            jopacoin_balance INTEGER NOT NULL DEFAULT 0,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (discord_id,guild_id)
        );
        CREATE TABLE nonprofit_fund (
            guild_id INTEGER PRIMARY KEY,
            total_collected INTEGER NOT NULL DEFAULT 0,
            next_match_pot INTEGER NOT NULL DEFAULT 0,
            first_game_open_pool INTEGER NOT NULL DEFAULT 0,
            first_game_lowskill_pool INTEGER NOT NULL DEFAULT 0,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE disburse_proposals (guild_id INTEGER,fund_amount INTEGER,status TEXT);
        CREATE TABLE pending_matches (guild_id INTEGER,pending_match_id INTEGER,
            PRIMARY KEY(guild_id,pending_match_id));
        CREATE TABLE first_game_pool_claims (
            guild_id INTEGER,pending_match_id INTEGER,amount INTEGER,settled INTEGER
        );
        CREATE TABLE predictions (guild_id INTEGER,status TEXT,lp_pnl INTEGER);
        CREATE TABLE duel_challenges (guild_id INTEGER,status TEXT,wager INTEGER);
        CREATE TABLE mafia_games (
            guild_id INTEGER,status TEXT,phase TEXT,entry_fee INTEGER,roster_size INTEGER
        );
        CREATE TABLE bets (
            guild_id INTEGER,pending_match_id INTEGER,amount INTEGER,leverage INTEGER
        );
        CREATE TABLE economy_policy_state (
            guild_id INTEGER PRIMARY KEY,
            mode TEXT NOT NULL DEFAULT 'normal'
                CHECK(mode IN ('recovery','normal','disabled')),
            target_annual_rate REAL NOT NULL DEFAULT 0.02,
            inflation_ceiling REAL NOT NULL DEFAULT 0.02,
            recovery_started_at INTEGER,
            stable_since INTEGER,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE economy_daily_snapshots (
            guild_id INTEGER NOT NULL,
            snapshot_date TEXT NOT NULL,
            captured_at INTEGER NOT NULL,
            player_wallets INTEGER NOT NULL,
            positive_wallets INTEGER NOT NULL,
            visible_debt INTEGER NOT NULL,
            player_count INTEGER NOT NULL,
            average_wallet REAL NOT NULL,
            reserve_available INTEGER NOT NULL,
            reserve_locked INTEGER NOT NULL,
            reserve_next_match_pot INTEGER NOT NULL,
            prediction_open_cash INTEGER NOT NULL,
            wager_escrow INTEGER NOT NULL,
            monetary_stock INTEGER NOT NULL,
            annualized_30d REAL,
            annualized_90d REAL,
            PRIMARY KEY(guild_id,snapshot_date)
        );
        CREATE TABLE economy_daily_events (
            event_id INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id INTEGER NOT NULL,
            event_date TEXT NOT NULL,
            name TEXT NOT NULL,
            hero TEXT NOT NULL,
            direction TEXT NOT NULL CHECK(direction IN ('deflationary','neutral','boon')),
            severity INTEGER NOT NULL CHECK(severity BETWEEN 1 AND 5),
            target_effect_jc INTEGER NOT NULL,
            forecast_flow_jc INTEGER NOT NULL,
            expected_effect_jc INTEGER NOT NULL,
            direct_effect_jc INTEGER NOT NULL DEFAULT 0,
            actual_stock_change_jc INTEGER,
            monetary_stock_before INTEGER NOT NULL,
            effects TEXT NOT NULL,
            announcement TEXT NOT NULL,
            starts_at INTEGER NOT NULL,
            ends_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            announced_at INTEGER,
            reminder_announced_at INTEGER,
            UNIQUE(guild_id,event_date)
        );
        CREATE TABLE economy_ledger_entries (
            ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id INTEGER NOT NULL,
            account_type TEXT NOT NULL CHECK(account_type IN ('player','nonprofit')),
            account_id INTEGER,
            delta INTEGER NOT NULL,
            balance_before INTEGER NOT NULL,
            balance_after INTEGER NOT NULL,
            source TEXT NOT NULL DEFAULT 'balance_update',
            actor_id INTEGER,
            related_type TEXT,
            related_id TEXT,
            reason TEXT,
            metadata TEXT,
            created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER))
        );
        CREATE TABLE economy_ledger_context (
            id INTEGER PRIMARY KEY CHECK(id=1),
            source TEXT,actor_id INTEGER,related_type TEXT,related_id TEXT,reason TEXT,metadata TEXT
        );
        CREATE TRIGGER trg_economy_ledger_players_update
        AFTER UPDATE OF jopacoin_balance ON players
        WHEN COALESCE(OLD.jopacoin_balance,0)!=COALESCE(NEW.jopacoin_balance,0)
        BEGIN
            INSERT INTO economy_ledger_entries (
                guild_id,account_type,account_id,delta,balance_before,balance_after,
                source,actor_id,related_type,related_id,reason,metadata
            ) VALUES (
                COALESCE(NEW.guild_id,0),'player',NEW.discord_id,
                COALESCE(NEW.jopacoin_balance,0)-COALESCE(OLD.jopacoin_balance,0),
                COALESCE(OLD.jopacoin_balance,0),COALESCE(NEW.jopacoin_balance,0),
                COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'balance_update'),
                (SELECT actor_id FROM economy_ledger_context WHERE id=1),
                (SELECT related_type FROM economy_ledger_context WHERE id=1),
                (SELECT related_id FROM economy_ledger_context WHERE id=1),
                (SELECT reason FROM economy_ledger_context WHERE id=1),
                (SELECT metadata FROM economy_ledger_context WHERE id=1)
            );
        END;
        CREATE TRIGGER trg_economy_ledger_nonprofit_update
        AFTER UPDATE OF total_collected ON nonprofit_fund
        WHEN COALESCE(OLD.total_collected,0)!=COALESCE(NEW.total_collected,0)
        BEGIN
            INSERT INTO economy_ledger_entries (
                guild_id,account_type,account_id,delta,balance_before,balance_after,
                source,actor_id,related_type,related_id,reason,metadata
            ) VALUES (
                COALESCE(NEW.guild_id,0),'nonprofit',COALESCE(NEW.guild_id,0),
                COALESCE(NEW.total_collected,0)-COALESCE(OLD.total_collected,0),
                COALESCE(OLD.total_collected,0),COALESCE(NEW.total_collected,0),
                COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'nonprofit_update'),
                (SELECT actor_id FROM economy_ledger_context WHERE id=1),
                (SELECT related_type FROM economy_ledger_context WHERE id=1),
                (SELECT related_id FROM economy_ledger_context WHERE id=1),
                (SELECT reason FROM economy_ledger_context WHERE id=1),
                (SELECT metadata FROM economy_ledger_context WHERE id=1)
            );
        END;
    "#;
}
