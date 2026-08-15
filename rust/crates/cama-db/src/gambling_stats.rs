//! Existing-schema gambling statistics and pure degen/leaderboard policy.
//!
//! Python remains the schema, migration, and live runtime authority during the
//! side-by-side rewrite. Production paths in this module only open an already
//! migrated database through [`crate::open_runtime_connection`]; they never
//! create or alter schema. The only multi-row write here records a Double or
//! Nothing spin and its player cooldown under `BEGIN IMMEDIATE`. Bet placement
//! and settlement remain owned by `betting_service_repository`, which already
//! guards its balance and bet-row transitions with the same boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, params, params_from_iter, types::Value,
};
use thiserror::Error;

use crate::open_runtime_connection;

const MAX_LEVERAGE_WEIGHT: i64 = 25;
const BET_SIZE_WEIGHT: i64 = 25;
const DEBT_DEPTH_WEIGHT: i64 = 20;
const BANKRUPTCY_WEIGHT: i64 = 15;
const FREQUENCY_WEIGHT: i64 = 10;
const LOSS_CHASE_WEIGHT: i64 = 5;
const NEGATIVE_LOAN_BONUS: i64 = 25;
const TYPICAL_INCOME_PER_MATCH: f64 = 3.0;
const MAX_DEBT: f64 = 500.0;
const SQLITE_ID_CHUNK: usize = 900;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BetSide {
    Radiant,
    Dire,
}

impl BetSide {
    fn parse(value: &str) -> Result<Self, GamblingStatsError> {
        match value {
            "radiant" => Ok(Self::Radiant),
            "dire" => Ok(Self::Dire),
            _ => Err(GamblingStatsError::InvalidBetSide(value.to_owned())),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }

    const fn won(self, winning_team: i64) -> bool {
        matches!((self, winning_team), (Self::Radiant, 1) | (Self::Dire, 2))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GamblingOutcome {
    Won,
    Lost,
    Neutral,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GamblingSource {
    Bet,
    Wheel,
    DoubleOrNothing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetHistoryEntry {
    pub bet_id: i64,
    pub amount: i64,
    pub leverage: i64,
    pub effective_bet: i64,
    pub team: BetSide,
    pub is_blind: bool,
    pub investment_target_id: Option<i64>,
    pub investment_direction: Option<String>,
    pub bet_time: i64,
    pub match_id: i64,
    pub payout: Option<i64>,
    pub vanity_tax: i64,
    pub outcome: GamblingOutcome,
    pub profit: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GamblingMetric {
    pub discord_id: i64,
    pub total_bets: i64,
    pub wins: i64,
    pub losses: i64,
    pub total_wagered: i64,
    pub net_pnl: i64,
    pub win_rate: f64,
    pub roi: f64,
    pub avg_leverage: f64,
    pub five_x_bets: i64,
    pub sequences_analyzed: i64,
    pub times_increased_after_loss: i64,
    pub unique_matches: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PaperHands {
    pub matches_played: i64,
    pub matches_bet_on_self: i64,
    pub paper_hands_count: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WheelSpin {
    pub result: i64,
    pub spin_time: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoubleOrNothingSpin {
    pub cost: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub won: bool,
    pub spin_time: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoubleOrNothingWrite {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub cost: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub won: bool,
    pub spin_time: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PnlEvent {
    pub amount: i64,
    pub leverage: i64,
    pub effective_bet: i64,
    pub outcome: GamblingOutcome,
    pub profit: i64,
    pub team: Option<BetSide>,
    pub source: GamblingSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PnlPoint {
    pub event_number: usize,
    pub cumulative_pnl: i64,
    pub event: PnlEvent,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AutoBetGroupStats {
    pub bet_count: usize,
    pub wins: usize,
    pub total_wagered: i64,
    pub net_pnl: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerAutoBetStats {
    pub target_id: i64,
    pub directions: Vec<String>,
    pub bet_count: usize,
    pub wins: usize,
    pub total_wagered: i64,
    pub net_pnl: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AutoBetPerformance {
    pub total: AutoBetGroupStats,
    pub generic: AutoBetGroupStats,
    pub targets: Vec<PlayerAutoBetStats>,
    pub arbitrage: AutoBetGroupStats,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DegenScoreBreakdown {
    pub total: i64,
    pub title: &'static str,
    pub emoji: &'static str,
    pub tagline: &'static str,
    pub max_leverage_score: i64,
    pub bet_size_score: i64,
    pub debt_depth_score: i64,
    pub bankruptcy_score: i64,
    pub frequency_score: i64,
    pub loss_chase_score: i64,
    pub negative_loan_bonus: i64,
    pub flavor_texts: Vec<String>,
}

impl DegenScoreBreakdown {
    #[must_use]
    pub fn zero() -> Self {
        compute_degen_score(&DegenScoreInputs::default())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DegenScoreInputs {
    pub leverage_distribution: BTreeMap<i64, i64>,
    pub total_bets: i64,
    pub total_wagered: i64,
    pub lowest_balance: Option<i64>,
    pub bankruptcy_count: i64,
    pub total_matches: i64,
    pub matches_bet_on: i64,
    pub loss_chase_rate: f64,
    pub negative_loans: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GambaStats {
    pub discord_id: i64,
    pub total_bets: i64,
    pub wins: i64,
    pub losses: i64,
    pub win_rate: f64,
    pub net_pnl: i64,
    pub total_wagered: i64,
    pub roi: f64,
    pub avg_bet_size: f64,
    pub leverage_distribution: BTreeMap<i64, i64>,
    pub current_streak: i64,
    pub best_streak: i64,
    pub worst_streak: i64,
    pub peak_pnl: i64,
    pub trough_pnl: i64,
    pub biggest_win: i64,
    pub biggest_loss: i64,
    pub degen_score: DegenScoreBreakdown,
    pub paper_hands_count: i64,
    pub matches_played: i64,
    pub auto_bet_performance: AutoBetPerformance,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LeaderboardEntry {
    pub discord_id: i64,
    pub total_bets: i64,
    pub wins: i64,
    pub losses: i64,
    pub win_rate: f64,
    pub net_pnl: i64,
    pub total_wagered: i64,
    pub avg_leverage: f64,
    pub degen_score: i64,
    pub degen_title: &'static str,
    pub degen_emoji: &'static str,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ServerStats {
    pub total_bets: i64,
    pub total_wagered: i64,
    pub unique_gamblers: usize,
    pub avg_bet_size: i64,
    pub total_bankruptcies: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Leaderboard {
    pub top_earners: Vec<LeaderboardEntry>,
    pub down_bad: Vec<LeaderboardEntry>,
    pub hall_of_degen: Vec<LeaderboardEntry>,
    pub biggest_gamblers: Vec<LeaderboardEntry>,
    pub total_wagered: i64,
    pub total_bets: i64,
    pub avg_degen_score: f64,
    pub total_bankruptcies: i64,
    pub total_loans: i64,
    pub server_stats: ServerStats,
}

/// One settled external wager used by the player-facing betting-impact
/// report.  This is deliberately a repository value object rather than a
/// second bet model: the source of truth remains the existing `bets` and
/// match-participant rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BettingImpactBet {
    pub bettor_id: i64,
    pub match_id: i64,
    pub effective_bet: i64,
    pub payout: Option<i64>,
    pub vanity_tax: i64,
    pub bet_for_target: bool,
    pub won: bool,
    pub profit: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BettorProfile {
    pub discord_id: i64,
    pub total_wagered_for: i64,
    pub total_wagered_against: i64,
    pub net_pnl_for: i64,
    pub net_pnl_against: i64,
    pub bets_for_count: i64,
    pub bets_against_count: i64,
    pub wins_for: i64,
    pub wins_against: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BettingImpactStats {
    pub discord_id: i64,
    pub matches_with_bets: i64,
    pub total_bets: i64,
    pub total_wagered_for: i64,
    pub total_wagered_against: i64,
    pub supporters_net_pnl: i64,
    pub haters_net_pnl: i64,
    pub supporter_win_rate: f64,
    pub hater_win_rate: f64,
    pub supporter_bets_count: i64,
    pub hater_bets_count: i64,
    pub market_favorability: f64,
    pub supporter_roi: f64,
    pub hater_roi: f64,
    pub biggest_fan: Option<BettorProfile>,
    pub biggest_hater: Option<BettorProfile>,
    pub most_consistent_fan: Option<BettorProfile>,
    pub most_consistent_hater: Option<BettorProfile>,
    pub blessing: Option<BettorProfile>,
    pub jinx: Option<BettorProfile>,
    pub luckiest_hater: Option<BettorProfile>,
    pub biggest_single_win: i64,
    pub biggest_single_loss: i64,
    pub unique_supporters: i64,
    pub unique_haters: i64,
}

#[derive(Debug, Error)]
pub enum GamblingStatsError {
    #[error("gambling statistics SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid persisted bet side {0:?}")]
    InvalidBetSide(String),
    #[error("gambling statistics arithmetic overflowed")]
    ArithmeticOverflow,
}

pub trait GamblingStatsPort {
    fn betting_impact_bets(
        &self,
        target_discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<BettingImpactBet>, GamblingStatsError>;

    fn player_bet_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<BetHistoryEntry>, GamblingStatsError>;

    fn paper_hands(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<PaperHands, GamblingStatsError>;

    fn player_bankruptcy_count(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, GamblingStatsError>;

    fn bulk_bankruptcy_counts(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, GamblingStatsError>;

    fn total_settled_matches(&self, guild_id: Option<i64>) -> Result<i64, GamblingStatsError>;

    fn lowest_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, GamblingStatsError>;

    fn lowest_balances_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Option<i64>>, GamblingStatsError>;

    fn negative_loans_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, GamblingStatsError>;

    fn total_loans_taken(&self, guild_id: Option<i64>) -> Result<i64, GamblingStatsError>;

    fn bulk_gambling_metrics(
        &self,
        guild_id: Option<i64>,
        discord_ids: Option<&[i64]>,
    ) -> Result<BTreeMap<i64, GamblingMetric>, GamblingStatsError>;

    fn wheel_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<WheelSpin>, GamblingStatsError>;

    fn double_or_nothing_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<DoubleOrNothingSpin>, GamblingStatsError>;
}

#[derive(Clone, Debug)]
pub struct GamblingStatsRepository {
    path: PathBuf,
}

impl GamblingStatsRepository {
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

    pub fn current_bet_streaks_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, GamblingStatsError> {
        let unique = unique_ids(discord_ids);
        if unique.is_empty() {
            return Ok(BTreeMap::new());
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut outcomes = BTreeMap::<i64, Vec<bool>>::new();
        for chunk in unique.chunks(SQLITE_ID_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT b.discord_id, b.team_bet_on, m.winning_team
                 FROM bets b JOIN matches m ON m.match_id = b.match_id
                 WHERE b.guild_id = ? AND b.match_id IS NOT NULL
                   AND b.discord_id IN ({placeholders})
                 ORDER BY b.discord_id, b.bet_time DESC, b.bet_id DESC"
            );
            let bindings = std::iter::once(guild_id).chain(chunk.iter().copied());
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(bindings), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (discord_id, side, winner) = row?;
                outcomes
                    .entry(discord_id)
                    .or_default()
                    .push(BetSide::parse(&side)?.won(winner));
            }
        }
        Ok(outcomes
            .into_iter()
            .filter_map(|(discord_id, values)| {
                let first = *values.first()?;
                let count = values.iter().take_while(|value| **value == first).count() as i64;
                Some((discord_id, if first { count } else { -count }))
            })
            .collect())
    }

    pub fn log_double_or_nothing_atomic(
        &self,
        write: DoubleOrNothingWrite,
    ) -> Result<i64, GamblingStatsError> {
        let guild_id = Self::normalize_guild_id(write.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO double_or_nothing_spins
                 (guild_id, discord_id, cost, balance_before, balance_after, won, spin_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                guild_id,
                write.discord_id,
                write.cost,
                write.balance_before,
                write.balance_after,
                i64::from(write.won),
                write.spin_time,
            ],
        )?;
        let spin_id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE players SET last_double_or_nothing = ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![write.spin_time, write.discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(spin_id)
    }
}

impl GamblingStatsPort for GamblingStatsRepository {
    fn betting_impact_bets(
        &self,
        target_discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<BettingImpactBet>, GamblingStatsError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut statement = connection.prepare(
            "SELECT b.discord_id, b.match_id,
                    b.amount * COALESCE(b.leverage, 1), b.payout,
                    COALESCE(bst.vanity_tax, 0),
                    CASE WHEN mp.team_number = 1 THEN 'radiant' ELSE 'dire' END,
                    b.team_bet_on,
                    m.winning_team
             FROM match_participants mp
             JOIN matches m ON m.match_id = mp.match_id
             JOIN bets b ON b.match_id = m.match_id
                AND b.discord_id != ?1 AND b.guild_id = ?2
             LEFT JOIN bet_settlement_taxes bst
                ON bst.match_id = b.match_id
               AND bst.guild_id = b.guild_id
               AND bst.discord_id = b.discord_id
             WHERE mp.discord_id = ?1 AND mp.guild_id = ?2
               AND m.guild_id = ?2 AND m.winning_team IS NOT NULL
             ORDER BY b.match_id, b.bet_time, b.bet_id",
        )?;
        let rows = statement
            .query_map(params![target_discord_id, guild_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut taxed_settlements = BTreeSet::new();
        rows.into_iter()
            .map(
                |(
                    bettor_id,
                    match_id,
                    effective_bet,
                    payout,
                    stored_vanity_tax,
                    target_team,
                    bet_team,
                    winning_team,
                )| {
                    let target_team = BetSide::parse(&target_team)?;
                    let bet_team = BetSide::parse(&bet_team)?;
                    let won = bet_team.won(winning_team);
                    let vanity_tax = if won && taxed_settlements.insert((bettor_id, match_id)) {
                        stored_vanity_tax
                    } else {
                        0
                    };
                    let profit = if won {
                        payout
                            .unwrap_or_else(|| effective_bet.saturating_mul(2))
                            .saturating_sub(effective_bet)
                            .saturating_sub(vanity_tax)
                    } else {
                        -effective_bet
                    };
                    Ok(BettingImpactBet {
                        bettor_id,
                        match_id,
                        effective_bet,
                        payout,
                        vanity_tax,
                        bet_for_target: bet_team == target_team,
                        won,
                        profit,
                    })
                },
            )
            .collect()
    }

    fn player_bet_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<BetHistoryEntry>, GamblingStatsError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT b.bet_id, b.amount, COALESCE(b.leverage, 1),
                    b.amount * COALESCE(b.leverage, 1), b.team_bet_on,
                    COALESCE(b.is_blind, 0), b.investment_target_id,
                    b.investment_direction, b.bet_time, b.match_id, b.payout,
                    COALESCE(bst.vanity_tax, 0), m.winning_team
             FROM bets b
             JOIN matches m ON m.match_id = b.match_id
             LEFT JOIN bet_settlement_taxes bst
               ON bst.match_id = b.match_id AND bst.guild_id = b.guild_id
              AND bst.discord_id = b.discord_id
             WHERE b.discord_id = ?1 AND b.guild_id = ?2 AND b.match_id IS NOT NULL
             ORDER BY b.bet_time, b.bet_id",
        )?;
        let rows = statement
            .query_map(
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, i64>(12)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut taxed_matches = BTreeSet::new();
        rows.into_iter()
            .map(|row| {
                let (
                    bet_id,
                    amount,
                    leverage,
                    effective_bet,
                    side,
                    is_blind,
                    investment_target_id,
                    investment_direction,
                    bet_time,
                    match_id,
                    payout,
                    stored_vanity_tax,
                    winning_team,
                ) = row;
                let team = BetSide::parse(&side)?;
                let won = team.won(winning_team);
                let vanity_tax = if won && taxed_matches.insert(match_id) {
                    stored_vanity_tax
                } else {
                    0
                };
                let profit = if won {
                    payout.unwrap_or(effective_bet.saturating_mul(2)) - effective_bet - vanity_tax
                } else {
                    -effective_bet
                };
                Ok(BetHistoryEntry {
                    bet_id,
                    amount,
                    leverage,
                    effective_bet,
                    team,
                    is_blind,
                    investment_target_id,
                    investment_direction,
                    bet_time,
                    match_id,
                    payout,
                    vanity_tax,
                    outcome: if won {
                        GamblingOutcome::Won
                    } else {
                        GamblingOutcome::Lost
                    },
                    profit,
                })
            })
            .collect()
    }

    fn paper_hands(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<PaperHands, GamblingStatsError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut statement = connection.prepare(
            "SELECT mp.match_id, mp.team_number, b.team_bet_on
             FROM match_participants mp
             LEFT JOIN bets b ON b.match_id = mp.match_id
                 AND b.guild_id = mp.guild_id AND b.discord_id = mp.discord_id
             WHERE mp.discord_id = ?1 AND mp.guild_id = ?2
             ORDER BY mp.match_id, b.bet_id",
        )?;
        let rows = statement
            .query_map(params![discord_id, guild_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut matches = BTreeMap::<i64, (i64, Option<String>)>::new();
        for (match_id, team_number, bet_side) in rows {
            matches
                .entry(match_id)
                .and_modify(|entry| {
                    if bet_side.is_some() {
                        entry.1 = bet_side.clone();
                    }
                })
                .or_insert((team_number, bet_side));
        }
        let matches_played = matches.len() as i64;
        let matches_bet_on_self = matches
            .values()
            .filter(|(team_number, side)| {
                side.as_deref() == Some(if *team_number == 1 { "radiant" } else { "dire" })
            })
            .count() as i64;
        Ok(PaperHands {
            matches_played,
            matches_bet_on_self,
            paper_hands_count: matches_played - matches_bet_on_self,
        })
    }

    fn player_bankruptcy_count(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, GamblingStatsError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT COALESCE(bankruptcy_count, 0) FROM bankruptcy_state
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    fn bulk_bankruptcy_counts(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, GamblingStatsError> {
        let unique = unique_ids(discord_ids);
        let mut result = unique
            .iter()
            .copied()
            .map(|discord_id| (discord_id, 0))
            .collect::<BTreeMap<_, _>>();
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        for chunk in unique.chunks(SQLITE_ID_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT discord_id, COALESCE(bankruptcy_count, 0)
                 FROM bankruptcy_state WHERE guild_id = ?
                   AND discord_id IN ({placeholders})"
            );
            let bindings = std::iter::once(guild_id).chain(chunk.iter().copied());
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(bindings), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (discord_id, count) = row?;
                result.insert(discord_id, count);
            }
        }
        Ok(result)
    }

    fn total_settled_matches(&self, guild_id: Option<i64>) -> Result<i64, GamblingStatsError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COUNT(*) FROM matches WHERE guild_id = ?1 AND winning_team IS NOT NULL",
                [Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn lowest_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, GamblingStatsError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT lowest_balance_ever FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    fn lowest_balances_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Option<i64>>, GamblingStatsError> {
        let unique = unique_ids(discord_ids);
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut result = BTreeMap::new();
        for chunk in unique.chunks(SQLITE_ID_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT discord_id, lowest_balance_ever FROM players
                 WHERE guild_id = ? AND discord_id IN ({placeholders})"
            );
            let bindings = std::iter::once(guild_id).chain(chunk.iter().copied());
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(bindings), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
            })?;
            result.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
        Ok(result)
    }

    fn negative_loans_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, GamblingStatsError> {
        let unique = unique_ids(discord_ids);
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut result = BTreeMap::new();
        for chunk in unique.chunks(SQLITE_ID_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT discord_id, COALESCE(negative_loans_taken, 0)
                 FROM loan_state WHERE guild_id = ? AND discord_id IN ({placeholders})"
            );
            let bindings = std::iter::once(guild_id).chain(chunk.iter().copied());
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(bindings), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            result.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
        Ok(result)
    }

    fn total_loans_taken(&self, guild_id: Option<i64>) -> Result<i64, GamblingStatsError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(SUM(total_loans_taken), 0) FROM loan_state WHERE guild_id = ?1",
                [Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn bulk_gambling_metrics(
        &self,
        guild_id: Option<i64>,
        discord_ids: Option<&[i64]>,
    ) -> Result<BTreeMap<i64, GamblingMetric>, GamblingStatsError> {
        let unique = discord_ids.map(unique_ids);
        if unique.as_ref().is_some_and(Vec::is_empty) {
            return Ok(BTreeMap::new());
        }
        let connection = self.connection()?;
        let chunks = unique.as_ref().map_or_else(
            || vec![None],
            |ids| ids.chunks(SQLITE_ID_CHUNK).map(Some).collect(),
        );
        let mut result = BTreeMap::new();
        for chunk in chunks {
            let (player_clause, mut bindings) = if let Some(chunk) = chunk {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(",");
                (
                    format!("AND b.discord_id IN ({placeholders})"),
                    std::iter::once(Value::Integer(Self::normalize_guild_id(guild_id)))
                        .chain(chunk.iter().copied().map(Value::Integer))
                        .collect::<Vec<_>>(),
                )
            } else {
                (
                    String::new(),
                    vec![Value::Integer(Self::normalize_guild_id(guild_id))],
                )
            };
            let sql = format!(
                "WITH base AS (
                     SELECT b.discord_id, b.guild_id, b.bet_id, b.match_id, b.bet_time,
                            b.amount * COALESCE(b.leverage, 1) AS effective_bet,
                            COALESCE(b.leverage, 1) AS leverage, b.payout,
                            CASE WHEN (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                                      OR (m.winning_team = 2 AND b.team_bet_on = 'dire')
                                 THEN 1 ELSE 0 END AS won
                     FROM bets b JOIN matches m ON b.match_id = m.match_id
                     WHERE b.guild_id = ? AND b.match_id IS NOT NULL {player_clause}
                 ), ordered AS (
                     SELECT *,
                            LAG(won) OVER (PARTITION BY discord_id ORDER BY bet_time, bet_id)
                                AS previous_won,
                            LAG(effective_bet) OVER (
                                PARTITION BY discord_id ORDER BY bet_time, bet_id
                            ) AS previous_effective_bet
                     FROM base
                 )
                 SELECT discord_id, COUNT(*), SUM(won), SUM(1 - won),
                        SUM(effective_bet), AVG(leverage),
                        SUM(CASE WHEN won = 1
                                 THEN COALESCE(payout, effective_bet * 2) - effective_bet
                                 ELSE -effective_bet END)
                          - COALESCE((SELECT SUM(bst.vanity_tax)
                                      FROM bet_settlement_taxes bst
                                      WHERE bst.guild_id = ordered.guild_id
                                        AND bst.discord_id = ordered.discord_id), 0),
                        COUNT(DISTINCT match_id),
                        SUM(CASE WHEN leverage = 5 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN previous_won = 0 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN previous_won = 0
                                      AND effective_bet > previous_effective_bet
                                 THEN 1 ELSE 0 END)
                 FROM ordered GROUP BY discord_id"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(bindings.drain(..)), |row| {
                let total_bets = row.get::<_, i64>(1)?;
                let total_wagered = row.get::<_, i64>(4)?;
                let net_pnl = row.get::<_, i64>(6)?;
                Ok(GamblingMetric {
                    discord_id: row.get(0)?,
                    total_bets,
                    wins: row.get(2)?,
                    losses: row.get(3)?,
                    total_wagered,
                    net_pnl,
                    win_rate: if total_bets == 0 {
                        0.0
                    } else {
                        row.get::<_, i64>(2)? as f64 / total_bets as f64
                    },
                    roi: if total_wagered == 0 {
                        0.0
                    } else {
                        net_pnl as f64 / total_wagered as f64
                    },
                    avg_leverage: row.get(5)?,
                    unique_matches: row.get(7)?,
                    five_x_bets: row.get(8)?,
                    sequences_analyzed: row.get(9)?,
                    times_increased_after_loss: row.get(10)?,
                })
            })?;
            for row in rows {
                let metric = row?;
                result.insert(metric.discord_id, metric);
            }
        }
        Ok(result)
    }

    fn wheel_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<WheelSpin>, GamblingStatsError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT result, spin_time FROM wheel_spins
             WHERE discord_id = ?1 AND guild_id = ?2 ORDER BY spin_time",
        )?;
        statement
            .query_map(
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(WheelSpin {
                        result: row.get(0)?,
                        spin_time: row.get(1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn double_or_nothing_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<DoubleOrNothingSpin>, GamblingStatsError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT cost, balance_before, balance_after, won, spin_time
             FROM double_or_nothing_spins
             WHERE discord_id = ?1 AND guild_id = ?2 ORDER BY spin_time",
        )?;
        statement
            .query_map(
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(DoubleOrNothingSpin {
                        cost: row.get(0)?,
                        balance_before: row.get(1)?,
                        balance_after: row.get(2)?,
                        won: row.get(3)?,
                        spin_time: row.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug)]
pub struct GamblingStatsService<P> {
    port: P,
}

impl<P> GamblingStatsService<P>
where
    P: GamblingStatsPort,
{
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn get_player_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<GambaStats>, GamblingStatsError> {
        let history = self.port.player_bet_history(discord_id, guild_id)?;
        if history.is_empty() {
            return Ok(None);
        }
        let total_bets = history.len() as i64;
        let wins = history
            .iter()
            .filter(|bet| bet.outcome == GamblingOutcome::Won)
            .count() as i64;
        let losses = total_bets - wins;
        let net_pnl = history.iter().map(|bet| bet.profit).sum::<i64>();
        let total_wagered = history.iter().map(|bet| bet.effective_bet).sum::<i64>();
        let leverage_distribution = leverage_distribution_from_history(&history);
        let (current_streak, best_streak, worst_streak) = calculate_streaks(&history);
        let (peak_pnl, trough_pnl) = calculate_pnl_extremes(&history);
        let paper_hands = self.port.paper_hands(discord_id, guild_id)?;
        let degen_score = self.calculate_degen_with_history(
            discord_id,
            guild_id,
            &history,
            &leverage_distribution,
        )?;
        Ok(Some(GambaStats {
            discord_id,
            total_bets,
            wins,
            losses,
            win_rate: wins as f64 / total_bets as f64,
            net_pnl,
            total_wagered,
            roi: if total_wagered == 0 {
                0.0
            } else {
                net_pnl as f64 / total_wagered as f64
            },
            avg_bet_size: total_wagered as f64 / total_bets as f64,
            leverage_distribution,
            current_streak,
            best_streak,
            worst_streak,
            peak_pnl,
            trough_pnl,
            biggest_win: history
                .iter()
                .map(|bet| bet.profit)
                .filter(|profit| *profit > 0)
                .max()
                .unwrap_or_default(),
            biggest_loss: history
                .iter()
                .map(|bet| bet.profit)
                .filter(|profit| *profit < 0)
                .min()
                .unwrap_or_default(),
            degen_score,
            paper_hands_count: paper_hands.paper_hands_count,
            matches_played: paper_hands.matches_played,
            auto_bet_performance: calculate_auto_bet_performance(&history),
        }))
    }

    /// Calculate the same external-bettor impact report used by the profile
    /// view.  The repository query has already enforced settled-match,
    /// guild, participant, and self-bet boundaries; this layer only folds the
    /// rows into the public profile metrics.
    pub fn get_betting_impact_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<BettingImpactStats>, GamblingStatsError> {
        let bets = self.port.betting_impact_bets(discord_id, guild_id)?;
        Ok(compute_betting_impact_stats(discord_id, &bets))
    }

    pub fn calculate_degen_score(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<DegenScoreBreakdown, GamblingStatsError> {
        self.calculate_degen_score_with_data(discord_id, guild_id, None, None)
    }

    pub fn calculate_degen_score_with_data(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        history: Option<Vec<BetHistoryEntry>>,
        leverage_distribution: Option<BTreeMap<i64, i64>>,
    ) -> Result<DegenScoreBreakdown, GamblingStatsError> {
        let history = match history {
            Some(history) => history,
            None => self.port.player_bet_history(discord_id, guild_id)?,
        };
        let leverage_distribution =
            leverage_distribution.unwrap_or_else(|| leverage_distribution_from_history(&history));
        self.calculate_degen_with_history(discord_id, guild_id, &history, &leverage_distribution)
    }

    fn calculate_degen_with_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        history: &[BetHistoryEntry],
        leverage_distribution: &BTreeMap<i64, i64>,
    ) -> Result<DegenScoreBreakdown, GamblingStatsError> {
        let (sequences, increased) = loss_chasing(history);
        let negative_loans = self
            .port
            .negative_loans_bulk(&[discord_id], guild_id)?
            .get(&discord_id)
            .copied()
            .unwrap_or_default();
        Ok(compute_degen_score(&DegenScoreInputs {
            leverage_distribution: leverage_distribution.clone(),
            total_bets: history.len() as i64,
            total_wagered: history.iter().map(|bet| bet.effective_bet).sum(),
            lowest_balance: self.port.lowest_balance(discord_id, guild_id)?,
            bankruptcy_count: self.port.player_bankruptcy_count(discord_id, guild_id)?,
            total_matches: self.port.total_settled_matches(guild_id)?,
            matches_bet_on: history
                .iter()
                .map(|bet| bet.match_id)
                .collect::<BTreeSet<_>>()
                .len() as i64,
            loss_chase_rate: if sequences == 0 {
                0.0
            } else {
                increased as f64 / sequences as f64
            },
            negative_loans,
        }))
    }

    pub fn calculate_degen_scores_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, DegenScoreBreakdown>, GamblingStatsError> {
        let unique = unique_ids(discord_ids);
        if unique.is_empty() {
            return Ok(BTreeMap::new());
        }
        let metrics = self.port.bulk_gambling_metrics(guild_id, Some(&unique))?;
        let bankruptcies = self.port.bulk_bankruptcy_counts(&unique, guild_id)?;
        let total_matches = self.port.total_settled_matches(guild_id)?;
        let lowest_balances = self.port.lowest_balances_bulk(&unique, guild_id)?;
        let negative_loans = self.port.negative_loans_bulk(&unique, guild_id)?;
        Ok(unique
            .into_iter()
            .map(|discord_id| {
                let score = score_from_metric(
                    metrics.get(&discord_id).copied().unwrap_or_default(),
                    bankruptcies.get(&discord_id).copied().unwrap_or_default(),
                    total_matches,
                    lowest_balances.get(&discord_id).copied().flatten(),
                    negative_loans.get(&discord_id).copied().unwrap_or_default(),
                );
                (discord_id, score)
            })
            .collect())
    }

    pub fn get_leaderboard(
        &self,
        guild_id: Option<i64>,
        limit: usize,
        min_bets: i64,
    ) -> Result<Leaderboard, GamblingStatsError> {
        let metrics = self.port.bulk_gambling_metrics(guild_id, None)?;
        let all_ids = metrics.keys().copied().collect::<Vec<_>>();
        let eligible_ids = metrics
            .values()
            .filter(|metric| metric.total_bets >= min_bets)
            .map(|metric| metric.discord_id)
            .collect::<Vec<_>>();
        let bankruptcies = self.port.bulk_bankruptcy_counts(&all_ids, guild_id)?;
        let total_matches = self.port.total_settled_matches(guild_id)?;
        let lowest_balances = self.port.lowest_balances_bulk(&eligible_ids, guild_id)?;
        let negative_loans = self.port.negative_loans_bulk(&eligible_ids, guild_id)?;
        let mut entries = eligible_ids
            .iter()
            .filter_map(|discord_id| metrics.get(discord_id))
            .map(|metric| {
                let score = score_from_metric(
                    *metric,
                    bankruptcies
                        .get(&metric.discord_id)
                        .copied()
                        .unwrap_or_default(),
                    total_matches,
                    lowest_balances.get(&metric.discord_id).copied().flatten(),
                    negative_loans
                        .get(&metric.discord_id)
                        .copied()
                        .unwrap_or_default(),
                );
                LeaderboardEntry {
                    discord_id: metric.discord_id,
                    total_bets: metric.total_bets,
                    wins: metric.wins,
                    losses: metric.losses,
                    win_rate: metric.win_rate,
                    net_pnl: metric.net_pnl,
                    total_wagered: metric.total_wagered,
                    avg_leverage: metric.avg_leverage,
                    degen_score: score.total,
                    degen_title: score.title,
                    degen_emoji: score.emoji,
                }
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.discord_id);
        let mut top_earners = entries
            .iter()
            .filter(|entry| entry.net_pnl >= 0)
            .cloned()
            .collect::<Vec<_>>();
        top_earners.sort_by_key(|entry| (std::cmp::Reverse(entry.net_pnl), entry.discord_id));
        top_earners.truncate(limit);
        let mut down_bad = entries
            .iter()
            .filter(|entry| entry.net_pnl < 0)
            .cloned()
            .collect::<Vec<_>>();
        down_bad.sort_by_key(|entry| (entry.net_pnl, entry.discord_id));
        down_bad.truncate(limit);
        let mut hall_of_degen = entries.clone();
        hall_of_degen.sort_by_key(|entry| (std::cmp::Reverse(entry.degen_score), entry.discord_id));
        hall_of_degen.truncate(limit);
        let mut biggest_gamblers = entries.clone();
        biggest_gamblers
            .sort_by_key(|entry| (std::cmp::Reverse(entry.total_wagered), entry.discord_id));
        biggest_gamblers.truncate(limit);
        let total_wagered = metrics.values().map(|metric| metric.total_wagered).sum();
        let total_bets = metrics.values().map(|metric| metric.total_bets).sum();
        let total_bankruptcies = bankruptcies.values().sum();
        let avg_degen_score = if entries.is_empty() {
            0.0
        } else {
            entries.iter().map(|entry| entry.degen_score).sum::<i64>() as f64 / entries.len() as f64
        };
        Ok(Leaderboard {
            top_earners,
            down_bad,
            hall_of_degen,
            biggest_gamblers,
            total_wagered,
            total_bets,
            avg_degen_score,
            total_bankruptcies,
            total_loans: self.port.total_loans_taken(guild_id)?,
            server_stats: ServerStats {
                total_bets,
                total_wagered,
                unique_gamblers: metrics.len(),
                avg_bet_size: if total_bets == 0 {
                    0
                } else {
                    total_wagered / total_bets
                },
                total_bankruptcies,
            },
        })
    }

    pub fn cumulative_pnl_series(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<PnlPoint>, GamblingStatsError> {
        let history = self.port.player_bet_history(discord_id, guild_id)?;
        let wheels = self.port.wheel_history(discord_id, guild_id)?;
        let double_or_nothing = self.port.double_or_nothing_history(discord_id, guild_id)?;
        let mut events = Vec::<(i64, PnlEvent)>::new();
        events.extend(history.into_iter().map(|bet| {
            (
                bet.bet_time,
                PnlEvent {
                    amount: bet.amount,
                    leverage: bet.leverage,
                    effective_bet: bet.effective_bet,
                    outcome: bet.outcome,
                    profit: bet.profit,
                    team: Some(bet.team),
                    source: GamblingSource::Bet,
                },
            )
        }));
        events.extend(wheels.into_iter().map(|spin| {
            let outcome = match spin.result.cmp(&0) {
                std::cmp::Ordering::Greater => GamblingOutcome::Won,
                std::cmp::Ordering::Less => GamblingOutcome::Lost,
                std::cmp::Ordering::Equal => GamblingOutcome::Neutral,
            };
            (
                spin.spin_time,
                PnlEvent {
                    amount: spin.result.saturating_abs(),
                    leverage: 1,
                    effective_bet: 0,
                    outcome,
                    profit: spin.result,
                    team: None,
                    source: GamblingSource::Wheel,
                },
            )
        }));
        events.extend(double_or_nothing.into_iter().map(|spin| {
            let original = spin.balance_before.saturating_add(spin.cost);
            (
                spin.spin_time,
                PnlEvent {
                    amount: original,
                    leverage: 1,
                    effective_bet: spin.balance_before,
                    outcome: if spin.won {
                        GamblingOutcome::Won
                    } else {
                        GamblingOutcome::Lost
                    },
                    profit: spin.balance_after.saturating_sub(original),
                    team: None,
                    source: GamblingSource::DoubleOrNothing,
                },
            )
        }));
        events.sort_by_key(|(time, _)| *time);
        let mut cumulative = 0_i64;
        events
            .into_iter()
            .enumerate()
            .map(|(index, (_, event))| {
                cumulative = cumulative
                    .checked_add(event.profit)
                    .ok_or(GamblingStatsError::ArithmeticOverflow)?;
                Ok(PnlPoint {
                    event_number: index + 1,
                    cumulative_pnl: cumulative,
                    event,
                })
            })
            .collect()
    }
}

/// Pure aggregation counterpart to [`GamblingStatsService::get_betting_impact_stats`].
/// Keeping this policy independent of SQLite makes the directional, rate, and
/// notable-bettor rules directly executable in migrated parity tests.
#[must_use]
pub fn compute_betting_impact_stats(
    discord_id: i64,
    bets: &[BettingImpactBet],
) -> Option<BettingImpactStats> {
    if bets.is_empty() {
        return None;
    }
    #[derive(Clone, Copy, Default)]
    struct Aggregate {
        wagered_for: i64,
        wagered_against: i64,
        pnl_for: i64,
        pnl_against: i64,
        bets_for: i64,
        bets_against: i64,
        wins_for: i64,
        wins_against: i64,
    }
    let mut aggregates = BTreeMap::<i64, Aggregate>::new();
    let mut matches = BTreeSet::new();
    let mut biggest_single_win = 0_i64;
    let mut biggest_single_loss = 0_i64;
    for bet in bets {
        matches.insert(bet.match_id);
        biggest_single_win = biggest_single_win.max(if bet.profit > 0 { bet.profit } else { 0 });
        biggest_single_loss = if bet.profit < 0 {
            biggest_single_loss.min(bet.profit)
        } else {
            biggest_single_loss
        };
        let aggregate = aggregates.entry(bet.bettor_id).or_default();
        if bet.bet_for_target {
            aggregate.wagered_for = aggregate.wagered_for.saturating_add(bet.effective_bet);
            aggregate.pnl_for = aggregate.pnl_for.saturating_add(bet.profit);
            aggregate.bets_for = aggregate.bets_for.saturating_add(1);
            if bet.won {
                aggregate.wins_for = aggregate.wins_for.saturating_add(1);
            }
        } else {
            aggregate.wagered_against = aggregate.wagered_against.saturating_add(bet.effective_bet);
            aggregate.pnl_against = aggregate.pnl_against.saturating_add(bet.profit);
            aggregate.bets_against = aggregate.bets_against.saturating_add(1);
            if bet.won {
                aggregate.wins_against = aggregate.wins_against.saturating_add(1);
            }
        }
    }
    let total_wagered_for: i64 = aggregates.values().map(|value| value.wagered_for).sum();
    let total_wagered_against: i64 = aggregates.values().map(|value| value.wagered_against).sum();
    let supporters_net_pnl: i64 = aggregates.values().map(|value| value.pnl_for).sum();
    let haters_net_pnl: i64 = aggregates.values().map(|value| value.pnl_against).sum();
    let supporter_bets_count: i64 = aggregates.values().map(|value| value.bets_for).sum();
    let hater_bets_count: i64 = aggregates.values().map(|value| value.bets_against).sum();
    let supporter_wins: i64 = aggregates.values().map(|value| value.wins_for).sum();
    let hater_wins: i64 = aggregates.values().map(|value| value.wins_against).sum();
    let supporters = aggregates
        .iter()
        .filter(|(_, value)| value.wagered_for > 0)
        .map(|(id, value)| (*id, *value))
        .collect::<BTreeMap<_, _>>();
    let haters = aggregates
        .iter()
        .filter(|(_, value)| value.wagered_against > 0)
        .map(|(id, value)| (*id, *value))
        .collect::<BTreeMap<_, _>>();
    let profile = |discord_id: i64, value: Aggregate| BettorProfile {
        discord_id,
        total_wagered_for: value.wagered_for,
        total_wagered_against: value.wagered_against,
        net_pnl_for: value.pnl_for,
        net_pnl_against: value.pnl_against,
        bets_for_count: value.bets_for,
        bets_against_count: value.bets_against,
        wins_for: value.wins_for,
        wins_against: value.wins_against,
    };
    let select = |values: &BTreeMap<i64, Aggregate>, key: fn(Aggregate) -> i64| {
        values
            .iter()
            .max_by(|(left_id, left), (right_id, right)| {
                key(**left)
                    .cmp(&key(**right))
                    .then_with(|| right_id.cmp(left_id))
            })
            .map(|(id, value)| profile(*id, *value))
    };
    let biggest_fan = select(&supporters, |value| value.wagered_for);
    let biggest_hater = select(&haters, |value| value.wagered_against);
    let most_consistent_fan = select(&supporters, |value| value.bets_for);
    let most_consistent_hater = select(&haters, |value| value.bets_against);
    let blessing = select(&supporters, |value| value.pnl_for).filter(|value| value.net_pnl_for > 0);
    let jinx = supporters
        .iter()
        .min_by(|(left_id, left), (right_id, right)| {
            left.pnl_for
                .cmp(&right.pnl_for)
                .then_with(|| left_id.cmp(right_id))
        })
        .map(|(id, value)| profile(*id, *value))
        .filter(|value| value.net_pnl_for < 0);
    let luckiest_hater =
        select(&haters, |value| value.pnl_against).filter(|value| value.net_pnl_against > 0);
    let total_wagered = total_wagered_for.saturating_add(total_wagered_against);
    Some(BettingImpactStats {
        discord_id,
        matches_with_bets: matches.len() as i64,
        total_bets: bets.len() as i64,
        total_wagered_for,
        total_wagered_against,
        supporters_net_pnl,
        haters_net_pnl,
        supporter_win_rate: if supporter_bets_count == 0 {
            0.0
        } else {
            supporter_wins as f64 / supporter_bets_count as f64
        },
        hater_win_rate: if hater_bets_count == 0 {
            0.0
        } else {
            hater_wins as f64 / hater_bets_count as f64
        },
        supporter_bets_count,
        hater_bets_count,
        market_favorability: if total_wagered == 0 {
            0.5
        } else {
            total_wagered_for as f64 / total_wagered as f64
        },
        supporter_roi: if total_wagered_for == 0 {
            0.0
        } else {
            supporters_net_pnl as f64 / total_wagered_for as f64
        },
        hater_roi: if total_wagered_against == 0 {
            0.0
        } else {
            haters_net_pnl as f64 / total_wagered_against as f64
        },
        biggest_fan,
        biggest_hater,
        most_consistent_fan,
        most_consistent_hater,
        blessing,
        jinx,
        luckiest_hater,
        biggest_single_win,
        biggest_single_loss,
        unique_supporters: supporters.len() as i64,
        unique_haters: haters.len() as i64,
    })
}

#[must_use]
pub fn leverage_distribution_from_history(history: &[BetHistoryEntry]) -> BTreeMap<i64, i64> {
    let mut result = BTreeMap::new();
    for bet in history {
        *result.entry(bet.leverage).or_default() += 1;
    }
    result
}

#[must_use]
pub fn leverage_distribution_from_nullable(values: &[Option<i64>]) -> BTreeMap<i64, i64> {
    let mut result = BTreeMap::new();
    for value in values {
        *result.entry(value.unwrap_or(1)).or_default() += 1;
    }
    result
}

#[must_use]
pub fn compute_degen_score(inputs: &DegenScoreInputs) -> DegenScoreBreakdown {
    let mut flavor_texts = Vec::new();
    let total_leverage_bets = inputs.leverage_distribution.values().sum::<i64>();
    let pct_5x = if total_leverage_bets == 0 {
        0.0
    } else {
        inputs
            .leverage_distribution
            .get(&5)
            .copied()
            .unwrap_or_default() as f64
            / total_leverage_bets as f64
    };
    let max_leverage_score =
        ((pct_5x * MAX_LEVERAGE_WEIGHT as f64) as i64).min(MAX_LEVERAGE_WEIGHT);
    if pct_5x >= 0.5 {
        flavor_texts.push("5x addict".to_owned());
    } else if pct_5x >= 0.2 {
        flavor_texts.push("5x enthusiast".to_owned());
    }
    let bet_ratio = if inputs.total_bets == 0 {
        0.0
    } else {
        inputs.total_wagered as f64 / inputs.total_bets as f64 / TYPICAL_INCOME_PER_MATCH
    };
    let bet_size_score = if inputs.total_bets == 0 {
        0
    } else {
        (((bet_ratio - 1.0) / 9.0 * BET_SIZE_WEIGHT as f64) as i64).clamp(0, BET_SIZE_WEIGHT)
    };
    if bet_ratio >= 10.0 {
        flavor_texts.push("high roller".to_owned());
    } else if bet_ratio >= 5.0 {
        flavor_texts.push("big bets".to_owned());
    }
    let debt_depth_score = inputs.lowest_balance.map_or(0, |balance| {
        if balance >= 0 {
            0
        } else {
            ((balance.saturating_abs() as f64 / MAX_DEBT).min(1.0) * DEBT_DEPTH_WEIGHT as f64)
                as i64
        }
    });
    if inputs.lowest_balance.is_some_and(|balance| balance <= -400) {
        flavor_texts.push("debt lord".to_owned());
    } else if inputs.lowest_balance.is_some_and(|balance| balance <= -200) {
        flavor_texts.push("deep in debt".to_owned());
    }
    let bankruptcy_score = inputs
        .bankruptcy_count
        .saturating_mul(5)
        .min(BANKRUPTCY_WEIGHT);
    if inputs.bankruptcy_count >= 3 {
        flavor_texts.push(format!("{} bankruptcies", inputs.bankruptcy_count));
    } else if inputs.bankruptcy_count > 0 {
        flavor_texts.push(format!("{} bankruptcy", inputs.bankruptcy_count));
    }
    let frequency_rate = if inputs.total_matches == 0 {
        0.0
    } else {
        inputs.matches_bet_on as f64 / inputs.total_matches as f64
    };
    let frequency_score = ((frequency_rate * FREQUENCY_WEIGHT as f64) as i64).min(FREQUENCY_WEIGHT);
    if frequency_rate >= 0.8 {
        flavor_texts.push("never misses".to_owned());
    }
    let loss_chase_score =
        ((inputs.loss_chase_rate * LOSS_CHASE_WEIGHT as f64) as i64).min(LOSS_CHASE_WEIGHT);
    if inputs.loss_chase_rate >= 0.5 {
        flavor_texts.push("loss chaser".to_owned());
    }
    let negative_loan_bonus = inputs.negative_loans.saturating_mul(NEGATIVE_LOAN_BONUS);
    if inputs.negative_loans >= 1 {
        flavor_texts.insert(
            0,
            format!("borrowed while broke x{}", inputs.negative_loans),
        );
    }
    let total = max_leverage_score
        .saturating_add(bet_size_score)
        .saturating_add(debt_depth_score)
        .saturating_add(bankruptcy_score)
        .saturating_add(frequency_score)
        .saturating_add(loss_chase_score)
        .saturating_add(negative_loan_bonus);
    let (title, tagline, emoji) = degen_tier(total.min(100));
    flavor_texts.truncate(3);
    DegenScoreBreakdown {
        total,
        title,
        emoji,
        tagline,
        max_leverage_score,
        bet_size_score,
        debt_depth_score,
        bankruptcy_score,
        frequency_score,
        loss_chase_score,
        negative_loan_bonus,
        flavor_texts,
    }
}

fn score_from_metric(
    metric: GamblingMetric,
    bankruptcy_count: i64,
    total_matches: i64,
    lowest_balance: Option<i64>,
    negative_loans: i64,
) -> DegenScoreBreakdown {
    let sequences = metric.sequences_analyzed;
    compute_degen_score(&DegenScoreInputs {
        leverage_distribution: BTreeMap::from([
            (5, metric.five_x_bets),
            (1, (metric.total_bets - metric.five_x_bets).max(0)),
        ]),
        total_bets: metric.total_bets,
        total_wagered: metric.total_wagered,
        lowest_balance,
        bankruptcy_count,
        total_matches,
        matches_bet_on: metric.unique_matches,
        loss_chase_rate: if sequences == 0 {
            0.0
        } else {
            metric.times_increased_after_loss as f64 / sequences as f64
        },
        negative_loans,
    })
}

fn calculate_streaks(history: &[BetHistoryEntry]) -> (i64, i64, i64) {
    let mut streak = 0_i64;
    let mut best = 0_i64;
    let mut worst = 0_i64;
    for bet in history {
        if bet.outcome == GamblingOutcome::Won {
            streak = if streak >= 0 { streak + 1 } else { 1 };
            best = best.max(streak);
        } else {
            streak = if streak <= 0 { streak - 1 } else { -1 };
            worst = worst.min(streak);
        }
    }
    (streak, best, worst)
}

fn calculate_pnl_extremes(history: &[BetHistoryEntry]) -> (i64, i64) {
    let mut cumulative = 0_i64;
    let mut peak = 0_i64;
    let mut trough = 0_i64;
    for bet in history {
        cumulative = cumulative.saturating_add(bet.profit);
        peak = peak.max(cumulative);
        trough = trough.min(cumulative);
    }
    (peak, trough)
}

fn loss_chasing(history: &[BetHistoryEntry]) -> (i64, i64) {
    let mut sequences = 0_i64;
    let mut increased = 0_i64;
    for pair in history.windows(2) {
        if pair[0].outcome == GamblingOutcome::Lost {
            sequences += 1;
            if pair[1].effective_bet > pair[0].effective_bet {
                increased += 1;
            }
        }
    }
    (sequences, increased)
}

fn calculate_auto_bet_performance(history: &[BetHistoryEntry]) -> AutoBetPerformance {
    let automatic = history
        .iter()
        .filter(|bet| bet.is_blind)
        .collect::<Vec<_>>();
    let generic = automatic
        .iter()
        .copied()
        .filter(|bet| bet.investment_target_id.is_none())
        .collect::<Vec<_>>();
    let mut by_target = BTreeMap::<i64, Vec<&BetHistoryEntry>>::new();
    for bet in &automatic {
        if let Some(target_id) = bet.investment_target_id {
            by_target.entry(target_id).or_default().push(bet);
        }
    }
    let auto_teams = automatic.iter().fold(
        BTreeMap::<i64, BTreeSet<BetSide>>::new(),
        |mut teams, bet| {
            teams.entry(bet.match_id).or_default().insert(bet.team);
            teams
        },
    );
    let arbitrage = history
        .iter()
        .filter(|bet| {
            !bet.is_blind
                && auto_teams.get(&bet.match_id).is_some_and(|teams| {
                    teams.contains(&match bet.team {
                        BetSide::Radiant => BetSide::Dire,
                        BetSide::Dire => BetSide::Radiant,
                    })
                })
        })
        .collect::<Vec<_>>();
    let mut targets = by_target
        .into_iter()
        .map(|(target_id, bets)| {
            let aggregate = auto_group(&bets);
            let directions = ["long", "short"]
                .into_iter()
                .filter(|direction| {
                    bets.iter()
                        .any(|bet| bet.investment_direction.as_deref() == Some(*direction))
                })
                .map(str::to_owned)
                .collect();
            PlayerAutoBetStats {
                target_id,
                directions,
                bet_count: aggregate.bet_count,
                wins: aggregate.wins,
                total_wagered: aggregate.total_wagered,
                net_pnl: aggregate.net_pnl,
            }
        })
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| {
        (
            std::cmp::Reverse(target.total_wagered),
            std::cmp::Reverse(target.bet_count),
            target.target_id,
        )
    });
    AutoBetPerformance {
        total: auto_group(&automatic),
        generic: auto_group(&generic),
        targets,
        arbitrage: auto_group(&arbitrage),
    }
}

fn auto_group(bets: &[&BetHistoryEntry]) -> AutoBetGroupStats {
    AutoBetGroupStats {
        bet_count: bets.len(),
        wins: bets
            .iter()
            .filter(|bet| bet.outcome == GamblingOutcome::Won)
            .count(),
        total_wagered: bets.iter().map(|bet| bet.effective_bet).sum(),
        net_pnl: bets.iter().map(|bet| bet.profit).sum(),
    }
}

const fn degen_tier(score: i64) -> (&'static str, &'static str, &'static str) {
    match score {
        0..=19 => ("Casual", "Do you even gamble?", "🥱"),
        20..=39 => ("Recreational", "Weekend warrior", "🎰"),
        40..=59 => ("Committed", "The house knows your name", "🔥"),
        60..=79 => ("Degenerate", "Your family is concerned", "💀"),
        80..=89 => ("Menace", "Financial advisor on suicide watch", "🎪"),
        90..=100 => ("Legendary Degen", "They write songs about you", "👑"),
        _ => ("Unknown", "???", "❓"),
    }
}

fn unique_ids(discord_ids: &[i64]) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    discord_ids
        .iter()
        .copied()
        .filter(|discord_id| seen.insert(*discord_id))
        .collect()
}

#[cfg(test)]
#[path = "gambling_stats/tests.rs"]
mod tests;
