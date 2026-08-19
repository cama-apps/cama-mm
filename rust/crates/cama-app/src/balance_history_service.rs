//! Unified, chart-ready Jopacoin balance history.
//!
//! The service keeps eight independent persistence reads behind a typed port,
//! reconstructs their actual balance deltas, merges them with stable timestamp
//! ordering, and exposes [`crate::drawing::BalancePoint`] values. Python
//! remains the live repository/runtime authority during parity.

use std::collections::BTreeMap;
use std::thread;

use thiserror::Error;

use crate::drawing::BalancePoint;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const SOURCE_BETS: &str = "bets";
pub const SOURCE_PREDICTIONS: &str = "predictions";
pub const SOURCE_WHEEL: &str = "wheel";
pub const SOURCE_DOUBLE_OR_NOTHING: &str = "double_or_nothing";
pub const SOURCE_TIPS: &str = "tips";
pub const SOURCE_DISBURSE: &str = "disburse";
pub const SOURCE_BONUS: &str = "bonus";
pub const SOURCE_DIG: &str = "dig";

pub const LEGACY_WIN_REWARD: i64 = 10;
pub const LEGACY_PARTICIPATION_REWARD: i64 = 5;

type EventCollector<'a> = Box<dyn Fn(DiscordId, GuildId) -> Vec<BalanceEvent> + Send + Sync + 'a>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetHistoryRow {
    pub bet_time: i64,
    pub profit: i64,
    pub won: bool,
    pub amount: i64,
    pub leverage: i64,
    pub match_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionHistoryRow {
    pub prediction_id: i64,
    pub settle_time: Option<i64>,
    pub delta: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WheelHistoryRow {
    pub spin_time: i64,
    pub result: i64,
    pub outcome_metadata: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoubleOrNothingHistoryRow {
    pub spin_time: i64,
    pub cost: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub won: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TipDirection {
    Sent,
    Received,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TipHistoryRow {
    pub timestamp: i64,
    pub amount: i64,
    pub fee: i64,
    pub direction: TipDirection,
    pub sender_id: DiscordId,
    pub recipient_id: DiscordId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisbursementHistoryRow {
    pub disbursed_at: i64,
    pub amount: i64,
    pub method: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchBonusHistoryRow {
    pub match_id: i64,
    pub match_time: Option<i64>,
    pub won: bool,
    pub bonus_jc: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigHistoryRow {
    pub actor_id: DiscordId,
    pub target_id: Option<DiscordId>,
    pub action_type: String,
    pub detail: Option<String>,
    pub created_at: i64,
    pub jc_delta: i64,
}

/// Raw persisted-history boundary. Concrete SQLite adapters may delegate to
/// the existing Python-compatible repositories without leaking row maps into
/// the reconstruction policy.
pub trait BalanceHistoryPort: Sync {
    fn bet_history(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BetHistoryRow>;

    fn prediction_history(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Vec<PredictionHistoryRow>;

    fn wheel_history(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<WheelHistoryRow>;

    fn double_or_nothing_history(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Vec<DoubleOrNothingHistoryRow>;

    fn tip_history(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<TipHistoryRow>;

    fn disbursement_history(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Vec<DisbursementHistoryRow>;

    fn match_bonus_history(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Vec<MatchBonusHistoryRow>;

    /// `None` means that no dig repository is installed; it is neutral.
    fn dig_history(&self, discord_id: DiscordId, guild_id: GuildId) -> Option<Vec<DigHistoryRow>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BonusComponents {
    PersistedNet(i64),
    Reconstructed { participation: i64, win: i64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventDetail {
    Bet {
        won: bool,
        amount: i64,
        leverage: i64,
        match_id: i64,
    },
    Prediction {
        won: bool,
        prediction_id: i64,
    },
    Wheel {
        won: bool,
        vanity_tax: i64,
    },
    DoubleOrNothing {
        won: bool,
        risked: i64,
    },
    Tip {
        direction: TipDirection,
        amount: i64,
        fee: i64,
    },
    Disbursement {
        method: String,
    },
    Bonus {
        match_id: i64,
        components: BonusComponents,
        won: bool,
    },
    Dig {
        action_type: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BalanceEvent {
    pub time: i64,
    pub delta: i64,
    pub source: &'static str,
    pub detail: EventDetail,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BalanceSeriesPoint {
    pub event_number: i32,
    pub cumulative: i64,
    pub event: BalanceEvent,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BalanceHistory {
    pub series: Vec<BalanceSeriesPoint>,
    pub source_totals: BTreeMap<&'static str, i64>,
}

impl BalanceHistory {
    #[must_use]
    pub fn chart_points(&self) -> Vec<BalancePoint> {
        self.series
            .iter()
            .map(|point| BalancePoint {
                event_number: point.event_number,
                cumulative: point.cumulative,
                source: point.event.source.to_owned(),
            })
            .collect()
    }

    #[must_use]
    pub fn chart_totals(&self) -> BTreeMap<String, i64> {
        self.source_totals
            .iter()
            .map(|(source, total)| ((*source).to_owned(), *total))
            .collect()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BalanceHistoryError {
    #[error("a balance-history source worker panicked")]
    WorkerPanic,
}

#[derive(Clone, Debug)]
pub struct BalanceHistoryService<P> {
    port: P,
}

impl<P> BalanceHistoryService<P>
where
    P: BalanceHistoryPort,
{
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }

    #[must_use]
    pub fn get_balance_event_series(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> BalanceHistory {
        merge_events(
            self.collectors()
                .into_iter()
                .flat_map(|collector| collector(discord_id, guild_id))
                .collect(),
        )
    }

    /// Run all eight independent source reads in parallel. Join order matches
    /// collector order, preserving synchronous stable ordering for equal
    /// timestamps.
    pub fn get_balance_event_series_concurrently(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<BalanceHistory, BalanceHistoryError> {
        let source_events = thread::scope(|scope| {
            let handles = self
                .collectors()
                .into_iter()
                .map(|collector| scope.spawn(move || collector(discord_id, guild_id)))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().map_err(|_| BalanceHistoryError::WorkerPanic))
                .collect::<Result<Vec<_>, _>>()
        })?;
        Ok(merge_events(source_events.into_iter().flatten().collect()))
    }

    fn collectors(&self) -> [EventCollector<'_>; 8] {
        [
            Box::new(|discord_id, guild_id| self.bet_events(discord_id, guild_id)),
            Box::new(|discord_id, guild_id| self.prediction_events(discord_id, guild_id)),
            Box::new(|discord_id, guild_id| self.wheel_events(discord_id, guild_id)),
            Box::new(|discord_id, guild_id| self.double_or_nothing_events(discord_id, guild_id)),
            Box::new(|discord_id, guild_id| self.tip_events(discord_id, guild_id)),
            Box::new(|discord_id, guild_id| self.disbursement_events(discord_id, guild_id)),
            Box::new(|discord_id, guild_id| self.bonus_events(discord_id, guild_id)),
            Box::new(|discord_id, guild_id| self.dig_events(discord_id, guild_id)),
        ]
    }

    fn bet_events(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BalanceEvent> {
        self.port
            .bet_history(discord_id, guild_id)
            .into_iter()
            .map(|row| BalanceEvent {
                time: row.bet_time,
                delta: row.profit,
                source: SOURCE_BETS,
                detail: EventDetail::Bet {
                    won: row.won,
                    amount: row.amount,
                    leverage: row.leverage,
                    match_id: row.match_id,
                },
            })
            .collect()
    }

    fn prediction_events(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BalanceEvent> {
        self.port
            .prediction_history(discord_id, guild_id)
            .into_iter()
            .filter(|row| row.delta != 0)
            .map(|row| BalanceEvent {
                time: row.settle_time.unwrap_or_default(),
                delta: row.delta,
                source: SOURCE_PREDICTIONS,
                detail: EventDetail::Prediction {
                    won: row.delta > 0,
                    prediction_id: row.prediction_id,
                },
            })
            .collect()
    }

    fn wheel_events(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BalanceEvent> {
        self.port
            .wheel_history(discord_id, guild_id)
            .into_iter()
            .filter_map(|row| {
                if row.result == 0 {
                    return None;
                }
                let withheld = |key: &str| {
                    row.outcome_metadata
                        .as_deref()
                        .and_then(|metadata| json_i64(metadata, key))
                        .unwrap_or_default()
                        .max(0)
                };
                let vanity_tax = withheld("vanity_tax");
                // A low-priority spinner is withheld a second, separately
                // recorded tax out of the same spin. Both must come off the
                // charted result or the wheel line over-reports the win.
                let net_result = row
                    .result
                    .saturating_sub(vanity_tax)
                    .saturating_sub(withheld("low_priority_tax"));
                Some(BalanceEvent {
                    time: row.spin_time,
                    delta: net_result,
                    source: SOURCE_WHEEL,
                    detail: EventDetail::Wheel {
                        won: net_result > 0,
                        vanity_tax,
                    },
                })
            })
            .collect()
    }

    fn double_or_nothing_events(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Vec<BalanceEvent> {
        self.port
            .double_or_nothing_history(discord_id, guild_id)
            .into_iter()
            .filter_map(|row| {
                let original = row.balance_before.saturating_add(row.cost);
                let delta = row.balance_after.saturating_sub(original);
                (delta != 0).then_some(BalanceEvent {
                    time: row.spin_time,
                    delta,
                    source: SOURCE_DOUBLE_OR_NOTHING,
                    detail: EventDetail::DoubleOrNothing {
                        won: row.won,
                        risked: row.balance_before,
                    },
                })
            })
            .collect()
    }

    fn tip_events(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BalanceEvent> {
        self.port
            .tip_history(discord_id, guild_id)
            .into_iter()
            .filter_map(|row| {
                let delta = if row.sender_id == row.recipient_id {
                    -row.fee
                } else if row.direction == TipDirection::Sent {
                    -row.amount.saturating_add(row.fee)
                } else {
                    row.amount
                };
                (delta != 0).then_some(BalanceEvent {
                    time: row.timestamp,
                    delta,
                    source: SOURCE_TIPS,
                    detail: EventDetail::Tip {
                        direction: row.direction,
                        amount: row.amount,
                        fee: row.fee,
                    },
                })
            })
            .collect()
    }

    fn disbursement_events(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BalanceEvent> {
        self.port
            .disbursement_history(discord_id, guild_id)
            .into_iter()
            .filter(|row| row.amount != 0)
            .map(|row| BalanceEvent {
                time: row.disbursed_at,
                delta: row.amount,
                source: SOURCE_DISBURSE,
                detail: EventDetail::Disbursement { method: row.method },
            })
            .collect()
    }

    fn bonus_events(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BalanceEvent> {
        self.port
            .match_bonus_history(discord_id, guild_id)
            .into_iter()
            .filter_map(|row| {
                let time = row.match_time?;
                let (delta, components) = match row.bonus_jc {
                    Some(delta) => (delta, BonusComponents::PersistedNet(delta)),
                    None if row.won => (
                        LEGACY_WIN_REWARD,
                        BonusComponents::Reconstructed {
                            participation: 0,
                            win: LEGACY_WIN_REWARD,
                        },
                    ),
                    None => (
                        LEGACY_PARTICIPATION_REWARD,
                        BonusComponents::Reconstructed {
                            participation: LEGACY_PARTICIPATION_REWARD,
                            win: 0,
                        },
                    ),
                };
                (delta != 0).then_some(BalanceEvent {
                    time,
                    delta,
                    source: SOURCE_BONUS,
                    detail: EventDetail::Bonus {
                        match_id: row.match_id,
                        components,
                        won: row.won,
                    },
                })
            })
            .collect()
    }

    fn dig_events(&self, discord_id: DiscordId, guild_id: GuildId) -> Vec<BalanceEvent> {
        self.port
            .dig_history(discord_id, guild_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| {
                let delta = dig_row_delta(discord_id, &row);
                (delta != 0).then_some(BalanceEvent {
                    time: row.created_at,
                    delta,
                    source: SOURCE_DIG,
                    detail: EventDetail::Dig {
                        action_type: row.action_type,
                    },
                })
            })
            .collect()
    }
}

#[must_use]
pub fn merge_events(mut events: Vec<BalanceEvent>) -> BalanceHistory {
    if events.is_empty() {
        return BalanceHistory::default();
    }
    events.sort_by_key(|event| event.time);
    let mut cumulative = 0_i64;
    let mut source_totals = BTreeMap::new();
    let series = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            cumulative = cumulative.saturating_add(event.delta);
            source_totals
                .entry(event.source)
                .and_modify(|total: &mut i64| *total = total.saturating_add(event.delta))
                .or_insert(event.delta);
            BalanceSeriesPoint {
                event_number: i32::try_from(index.saturating_add(1)).unwrap_or(i32::MAX),
                cumulative,
                event,
            }
        })
        .collect();
    source_totals.retain(|_, total| *total != 0);
    BalanceHistory {
        series,
        source_totals,
    }
}

#[must_use]
pub fn dig_row_delta(discord_id: DiscordId, row: &DigHistoryRow) -> i64 {
    let is_actor = row.actor_id == discord_id;
    let is_target = row.target_id == Some(discord_id);
    if row.jc_delta != 0 && is_actor {
        return row.jc_delta;
    }
    let detail = row.detail.as_deref().unwrap_or("{}");
    match row.action_type.as_str() {
        "dig" if is_actor => json_i64(detail, "jc").unwrap_or_default(),
        "help" if is_actor => 1,
        "sabotage" => {
            let trap_triggered = json_bool(detail, "trap_triggered").unwrap_or(false);
            if is_actor && trap_triggered {
                -json_i64(detail, "jc_lost").unwrap_or_default()
            } else if is_actor {
                -json_i64(detail, "cost").unwrap_or_default()
            } else if is_target && trap_triggered {
                json_i64(detail, "jc_lost").unwrap_or_default() / 2
            } else {
                0
            }
        }
        "boss_fight" if is_actor => {
            let won = json_bool(detail, "won").unwrap_or(false);
            let wager = json_i64(detail, "wager").unwrap_or_default();
            if won {
                let gross = json_i64(detail, "jc_delta").unwrap_or_default();
                if wager > 0 && gross > 0 {
                    gross.saturating_sub(wager)
                } else {
                    gross
                }
            } else {
                -wager
            }
        }
        "boss_retreat" if is_actor => -json_i64(detail, "loss").unwrap_or_default(),
        "event" if is_actor => {
            if json_bool(detail, "cruel_echoes").unwrap_or(false) {
                -1
            } else {
                json_i64(detail, "jc_delta")
                    .or_else(|| json_i64(detail, "jc"))
                    .unwrap_or_default()
            }
        }
        "abandon" if is_actor => json_i64(detail, "refund").unwrap_or_default(),
        _ => 0,
    }
}

fn json_value<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\"");
    let start = raw.find(&marker)? + marker.len();
    let remainder = raw[start..].trim_start().strip_prefix(':')?.trim_start();
    let end = remainder.find([',', '}']).unwrap_or(remainder.len());
    Some(remainder[..end].trim())
}

fn json_i64(raw: &str, key: &str) -> Option<i64> {
    json_value(raw, key)?.parse().ok()
}

fn json_bool(raw: &str, key: &str) -> Option<bool> {
    match json_value(raw, key)? {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "balance_history_service/tests.rs"]
mod tests;
