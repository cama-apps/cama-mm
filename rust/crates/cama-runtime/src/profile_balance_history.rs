//! SQLite adapter for the profile Economy balance-history chart.
//!
//! The reconstruction policy and renderer live in `cama-app`; this module only
//! translates the existing Python-owned SQLite tables into that typed port.
//! Reads are deliberately defensive because a legacy database can predate one
//! of the optional economy tables. A missing source is neutral, matching the
//! Python chart builder's best-effort behavior.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cama_app::balance_history_service::{
    BalanceHistoryPort, BetHistoryRow, DigHistoryRow, DisbursementHistoryRow,
    DoubleOrNothingHistoryRow, MatchBonusHistoryRow, PredictionHistoryRow, TipDirection,
    TipHistoryRow, WheelHistoryRow,
};
use cama_db::gambling_stats_repository::{
    GamblingOutcome, GamblingStatsPort, GamblingStatsRepository,
};
use cama_db::predictions_repository::CONTRACT_VALUE;
use cama_db::tip_repository::{
    DirectedTipTransaction, TipDirection as RepositoryTipDirection, TipRepository,
};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct SqliteBalanceHistoryPort {
    path: PathBuf,
    gambling: GamblingStatsRepository,
    tips: TipRepository,
}

impl SqliteBalanceHistoryPort {
    pub(crate) fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            gambling: GamblingStatsRepository::new(&path),
            tips: TipRepository::new(&path),
            path,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        // Match the Python/runtime compatibility policy. This is a read-only
        // adapter, but the pragma avoids legacy foreign-key mismatches while
        // the connection is opened alongside the existing repositories.
        connection.pragma_update(None, "foreign_keys", false)?;
        Ok(connection)
    }

    fn query_wheel_history(&self, discord_id: i64, guild_id: i64) -> Vec<WheelHistoryRow> {
        let Ok(connection) = self.connection() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT result, spin_time, outcome_metadata
             FROM wheel_spins
             WHERE discord_id = ?1 AND guild_id = ?2
             ORDER BY spin_time ASC",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map(params![discord_id, guild_id], |row| {
            Ok(WheelHistoryRow {
                result: row.get(0)?,
                spin_time: row.get(1)?,
                outcome_metadata: row.get(2)?,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    fn query_prediction_history(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Vec<PredictionHistoryRow> {
        let Ok(connection) = self.connection() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT p.prediction_id,
                    p.resolved_at,
                    p.outcome,
                    pp.yes_contracts,
                    pp.no_contracts,
                    pp.yes_cost_basis_total + pp.no_cost_basis_total AS cost,
                    COALESCE(pp.bankruptcy_penalty, 0) AS penalty,
                    COALESCE(pp.vanity_tax, 0) AS vanity_tax,
                    COALESCE(pp.low_priority_tax, 0) AS low_priority_tax,
                    (SELECT COUNT(*)
                     FROM economy_ledger_entries e
                     WHERE e.guild_id = p.guild_id
                       AND e.source = 'prediction_resolution'
                       AND e.related_type = 'prediction'
                       AND e.related_id = CAST(p.prediction_id AS TEXT)
                       AND e.ledger_id > COALESCE((
                           SELECT MAX(rb.ledger_id)
                           FROM economy_ledger_entries rb
                           WHERE rb.guild_id = p.guild_id
                             AND rb.source = 'prediction_resolution_rollback'
                             AND rb.related_type = 'prediction'
                             AND rb.related_id = CAST(p.prediction_id AS TEXT)
                       ), 0)) AS settlement_ledger_count,
                    (SELECT COALESCE(SUM(e.delta), 0)
                     FROM economy_ledger_entries e
                     WHERE e.guild_id = p.guild_id
                       AND e.account_type = 'player'
                       AND e.account_id = pp.discord_id
                       AND e.source IN ('prediction_resolution', 'vanity_tax',
                                       'low_priority_tax')
                       AND e.related_type = 'prediction'
                       AND e.related_id = CAST(p.prediction_id AS TEXT)
                       AND e.ledger_id > COALESCE((
                           SELECT MAX(rb.ledger_id)
                           FROM economy_ledger_entries rb
                           WHERE rb.guild_id = p.guild_id
                             AND rb.source = 'prediction_resolution_rollback'
                             AND rb.related_type = 'prediction'
                             AND rb.related_id = CAST(p.prediction_id AS TEXT)
                       ), 0)) AS credited
             FROM prediction_positions pp
             JOIN predictions p ON pp.prediction_id = p.prediction_id
             WHERE pp.discord_id = ?1 AND p.guild_id = ?2 AND p.status = 'resolved'
             ORDER BY p.resolved_at ASC",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map(params![discord_id, guild_id], |row| {
            let outcome: String = row.get(2)?;
            let yes_contracts: i64 = row.get(3)?;
            let no_contracts: i64 = row.get(4)?;
            let cost: i64 = row.get(5)?;
            let penalty: i64 = row.get(6)?;
            let vanity_tax: i64 = row.get(7)?;
            let low_priority_tax: i64 = row.get(8)?;
            let settlement_ledger_count: i64 = row.get(9)?;
            let credited: i64 = row.get(10)?;
            let winning_contracts = if outcome == "yes" {
                yes_contracts
            } else {
                no_contracts
            };
            let payout = if settlement_ledger_count > 0 {
                credited
            } else {
                winning_contracts
                    .saturating_mul(CONTRACT_VALUE)
                    .saturating_sub(penalty)
                    .saturating_sub(vanity_tax)
                    .saturating_sub(low_priority_tax)
            };
            Ok(PredictionHistoryRow {
                prediction_id: row.get(0)?,
                settle_time: row.get(1)?,
                delta: payout.saturating_sub(cost),
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    fn query_disbursement_history(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Vec<DisbursementHistoryRow> {
        let Ok(connection) = self.connection() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT disbursed_at, method, recipients
             FROM disburse_history
             WHERE guild_id = ?1
             ORDER BY disbursed_at DESC",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map(params![guild_id], |row| {
            let disbursed_at: i64 = row.get(0)?;
            let method: String = row.get(1)?;
            let recipients: String = row.get(2)?;
            Ok((disbursed_at, method, recipients))
        }) else {
            return Vec::new();
        };

        rows.filter_map(Result::ok)
            .filter_map(|(disbursed_at, method, recipients)| {
                let parsed = serde_json::from_str::<Value>(&recipients).ok()?;
                let amount = parsed.as_array()?.iter().find_map(|entry| {
                    let pair = entry.as_array()?;
                    let recipient = pair.first()?.as_i64()?;
                    let amount = pair.get(1)?.as_i64()?;
                    (recipient == discord_id && amount != 0).then_some(amount)
                })?;
                Some(DisbursementHistoryRow {
                    disbursed_at,
                    amount,
                    method,
                })
            })
            .collect()
    }

    fn query_match_bonus_history(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Vec<MatchBonusHistoryRow> {
        let Ok(connection) = self.connection() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT m.match_id,
                    CAST(strftime('%s', m.match_date) AS INTEGER) AS match_time,
                    COALESCE(mp.won, 0),
                    mp.bonus_jc
             FROM match_participants mp
             JOIN matches m ON mp.match_id = m.match_id
             WHERE mp.discord_id = ?1 AND mp.guild_id = ?2
             ORDER BY m.match_date ASC",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map(params![discord_id, guild_id], |row| {
            Ok(MatchBonusHistoryRow {
                match_id: row.get(0)?,
                match_time: row.get(1)?,
                won: row.get(2)?,
                bonus_jc: row.get(3)?,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    fn query_dig_history(&self, discord_id: i64, guild_id: i64) -> Vec<DigHistoryRow> {
        let Ok(connection) = self.connection() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT actor_id, target_id, action_type, detail, created_at, jc_delta
             FROM (
                 SELECT actor_id, target_id, action_type, detail, created_at, jc_delta
                 FROM dig_actions
                 WHERE guild_id = ?1 AND actor_id = ?2

                 UNION ALL

                 SELECT actor_id, target_id, action_type, detail, created_at, jc_delta
                 FROM dig_actions
                 WHERE guild_id = ?3 AND target_id = ?4 AND actor_id <> ?5
             ) AS player_actions
             ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map(
            params![guild_id, discord_id, guild_id, discord_id, discord_id],
            |row| {
                Ok(DigHistoryRow {
                    actor_id: row.get(0)?,
                    target_id: row.get(1)?,
                    action_type: row.get(2)?,
                    detail: row.get(3)?,
                    created_at: row.get(4)?,
                    jc_delta: row.get(5)?,
                })
            },
        ) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }
}

impl BalanceHistoryPort for SqliteBalanceHistoryPort {
    fn bet_history(&self, discord_id: i64, guild_id: i64) -> Vec<BetHistoryRow> {
        self.gambling
            .player_bet_history(discord_id, Some(guild_id))
            .unwrap_or_default()
            .into_iter()
            .map(|row| BetHistoryRow {
                bet_time: row.bet_time,
                profit: row.profit,
                won: matches!(row.outcome, GamblingOutcome::Won),
                amount: row.amount,
                leverage: row.leverage,
                match_id: row.match_id,
            })
            .collect()
    }

    fn prediction_history(&self, discord_id: i64, guild_id: i64) -> Vec<PredictionHistoryRow> {
        self.query_prediction_history(discord_id, guild_id)
    }

    fn wheel_history(&self, discord_id: i64, guild_id: i64) -> Vec<WheelHistoryRow> {
        self.query_wheel_history(discord_id, guild_id)
    }

    fn double_or_nothing_history(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Vec<DoubleOrNothingHistoryRow> {
        self.gambling
            .double_or_nothing_history(discord_id, Some(guild_id))
            .unwrap_or_default()
            .into_iter()
            .map(|row| DoubleOrNothingHistoryRow {
                spin_time: row.spin_time,
                cost: row.cost,
                balance_before: row.balance_before,
                balance_after: row.balance_after,
                won: row.won,
            })
            .collect()
    }

    fn tip_history(&self, discord_id: i64, guild_id: i64) -> Vec<TipHistoryRow> {
        self.tips
            .get_all_tips_for_user(discord_id, Some(guild_id))
            .unwrap_or_default()
            .into_iter()
            .map(directed_tip_row)
            .collect()
    }

    fn disbursement_history(&self, discord_id: i64, guild_id: i64) -> Vec<DisbursementHistoryRow> {
        self.query_disbursement_history(discord_id, guild_id)
    }

    fn match_bonus_history(&self, discord_id: i64, guild_id: i64) -> Vec<MatchBonusHistoryRow> {
        self.query_match_bonus_history(discord_id, guild_id)
    }

    fn dig_history(&self, discord_id: i64, guild_id: i64) -> Option<Vec<DigHistoryRow>> {
        Some(self.query_dig_history(discord_id, guild_id))
    }
}

fn directed_tip_row(row: DirectedTipTransaction) -> TipHistoryRow {
    let direction = match row.direction {
        RepositoryTipDirection::Sent => TipDirection::Sent,
        RepositoryTipDirection::Received => TipDirection::Received,
    };
    TipHistoryRow {
        timestamp: row.tip.timestamp,
        amount: row.tip.amount,
        fee: row.tip.fee,
        direction,
        sender_id: row.tip.sender_id,
        recipient_id: row.tip.recipient_id,
    }
}
