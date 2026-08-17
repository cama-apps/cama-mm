//! Existing-schema persistence for match finalization.
//!
//! Python remains the migration and production runtime authority. This module
//! opens only an existing database through [`crate::open_runtime_connection`]
//! and applies its persisted match slice in one `BEGIN IMMEDIATE` transaction:
//! match/participants, stats and rating history, rewards, house bets, and
//! participant loan repayment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::{json_numeric::coerce_i64_like_python, open_runtime_connection};

pub const DEFAULT_WIN_REWARD: i64 = 10;
pub const DEFAULT_PARTICIPATION_REWARD: i64 = 5;
pub const DEFAULT_EXCLUSION_REWARD: i64 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TeamSide {
    Radiant,
    Dire,
}

impl TeamSide {
    #[must_use]
    pub const fn number(self) -> i64 {
        match self {
            Self::Radiant => 1,
            Self::Dire => 2,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "radiant" => Some(Self::Radiant),
            "dire" => Some(Self::Dire),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum MatchRecordingRepositoryError {
    #[error("match-recording SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid team composition: {0}")]
    InvalidTeams(&'static str),
    #[error("player {0} is not registered in this guild")]
    MissingPlayer(i64),
    #[error("player {discord_id} has insufficient balance for a bet of {amount}")]
    InsufficientBalance { discord_id: i64, amount: i64 },
    #[error("player {0} already has an outstanding loan")]
    OutstandingLoan(i64),
    #[error("loan amount must be positive")]
    InvalidLoanAmount,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerRecord {
    pub discord_id: i64,
    pub name: String,
    pub balance: i64,
    pub wins: i64,
    pub losses: i64,
    pub glicko_rating: f64,
    pub glicko_rd: f64,
    pub glicko_volatility: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantRecord {
    pub discord_id: i64,
    pub team_number: i64,
    pub won: bool,
    pub side: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMatch {
    pub match_id: i64,
    pub radiant_ids: Vec<i64>,
    pub dire_ids: Vec<i64>,
    pub winning_team: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingHistoryRecord {
    pub discord_id: i64,
    pub rating: f64,
    pub rating_before: f64,
    pub won: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetResult {
    pub discord_id: i64,
    pub team: TeamSide,
    pub amount: i64,
    pub gross_credit: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BetDistributions {
    pub winners: Vec<BetResult>,
    pub losers: Vec<BetResult>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JcChange {
    pub payout: i64,
    pub bet: i64,
}

/// One generated-JC credit resolved against the player's live balance.
///
/// The rates intentionally remain floating-point values: Python truncates
/// each product with `int(...)`, and production configuration accepts rates
/// that cannot be represented exactly as basis points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IncomeAwardRequest<'a> {
    pub discord_id: i64,
    pub gross: i64,
    pub garnishment_rate: f64,
    pub apply_bankruptcy_penalty: bool,
    /// Fraction of post-garnishment income retained while penalized.
    pub bankruptcy_kept_rate: f64,
    pub vanity_tax_rate: f64,
    pub decrement_bankruptcy_penalty: bool,
    pub source: &'a str,
    pub related_type: Option<&'a str>,
    pub related_id: Option<i64>,
    pub reason: &'a str,
    /// Treat `(player, source, related_type, related_id)` as the durable
    /// marker for an already-applied credit.
    pub exact_once: bool,
}

impl<'a> IncomeAwardRequest<'a> {
    #[must_use]
    pub const fn ordinary(discord_id: i64, gross: i64, source: &'a str) -> Self {
        Self {
            discord_id,
            gross,
            garnishment_rate: 0.0,
            apply_bankruptcy_penalty: false,
            bankruptcy_kept_rate: 1.0,
            vanity_tax_rate: 0.0,
            decrement_bankruptcy_penalty: false,
            source,
            related_type: None,
            related_id: None,
            reason: source,
            exact_once: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IncomeAwardReceipt {
    pub discord_id: i64,
    pub gross: i64,
    pub garnished: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    /// Spendable income after every deduction, matching Python's `net` key.
    pub net: i64,
    /// Actual player-balance movement. Garnishment is a bookkeeping split,
    /// so this is `net + garnished`.
    pub balance_delta: i64,
    pub balance_after: i64,
    pub penalty_games_remaining: i64,
    pub applied: bool,
}

/// A balance delta that reverses one exact-once generated-income phase.
///
/// Match reward retries need to distinguish a compensated award from an
/// already-completed award.  The compensation is recorded as its own ledger
/// row, so the original award remains auditable while the next attempt is
/// allowed to apply the source again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncomeAwardCompensation<'a> {
    pub discord_id: i64,
    pub source: &'a str,
    pub related_id: i64,
    pub amount: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoanRepayment {
    pub player_id: i64,
    pub principal: i64,
    pub fee: i64,
    pub total_repaid: i64,
    pub balance_before: i64,
    pub new_balance: i64,
    pub nonprofit_total: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchFinalization {
    pub match_id: i64,
    pub winning_team: TeamSide,
    pub winning_player_ids: Vec<i64>,
    pub losing_player_ids: Vec<i64>,
    pub excluded_player_ids: Vec<i64>,
    pub updated_count: usize,
    pub bet_distributions: BetDistributions,
    pub jc_changes: BTreeMap<i64, JcChange>,
    pub loan_repayments: Vec<LoanRepayment>,
}

#[derive(Clone, Copy, Debug)]
pub struct MatchRecordRequest<'a> {
    pub guild_id: Option<i64>,
    pub radiant_ids: &'a [i64],
    pub dire_ids: &'a [i64],
    pub winning_team: TeamSide,
    /// Bench players who receive the ordinary exclusion reward.
    pub rewarded_excluded_ids: &'a [i64],
    /// Conditional nonresponders are tracked by orchestration and omitted here.
    pub all_excluded_ids: &'a [i64],
    pub pending_bet_since: i64,
    pub win_reward: i64,
    pub participation_reward: i64,
    pub exclusion_reward: i64,
    pub update_ratings: bool,
    /// Durable Python `pending_matches` identity used by record retries.
    pub pending_match_id: Option<i64>,
    pub exclusion_decay_ids: &'a [i64],
    pub full_exclusion_increment_ids: &'a [i64],
    pub half_exclusion_increment_ids: &'a [i64],
}

impl<'a> MatchRecordRequest<'a> {
    #[must_use]
    pub const fn standard(
        guild_id: Option<i64>,
        radiant_ids: &'a [i64],
        dire_ids: &'a [i64],
        winning_team: TeamSide,
    ) -> Self {
        Self {
            guild_id,
            radiant_ids,
            dire_ids,
            winning_team,
            rewarded_excluded_ids: &[],
            all_excluded_ids: &[],
            pending_bet_since: 0,
            win_reward: DEFAULT_WIN_REWARD,
            participation_reward: DEFAULT_PARTICIPATION_REWARD,
            exclusion_reward: DEFAULT_EXCLUSION_REWARD,
            update_ratings: true,
            pending_match_id: None,
            exclusion_decay_ids: &[],
            full_exclusion_increment_ids: &[],
            half_exclusion_increment_ids: &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnrichmentParticipantUpdate {
    pub discord_id: i64,
    pub hero_id: Option<i64>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub gpm: Option<i64>,
    pub xpm: Option<i64>,
    pub hero_damage: Option<i64>,
    pub tower_damage: Option<i64>,
    pub last_hits: Option<i64>,
    pub denies: Option<i64>,
    pub net_worth: Option<i64>,
    pub hero_healing: Option<i64>,
    pub lane_role: Option<i64>,
    pub lane_efficiency: Option<i64>,
    /// Gold at ten minutes, used to split each lane into core and support.
    pub gold_at_10: Option<i64>,
    pub towers_killed: Option<i64>,
    pub roshans_killed: Option<i64>,
    pub teamfight_participation: Option<f64>,
    pub obs_placed: Option<i64>,
    pub sen_placed: Option<i64>,
    pub camps_stacked: Option<i64>,
    pub rune_pickups: Option<i64>,
    pub firstblood_claimed: Option<i64>,
    pub stuns: Option<f64>,
    pub fantasy_points: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WrappedEnrichmentFactUpdate {
    pub discord_id: i64,
    pub actions_per_min: Option<i64>,
    pub courier_kills: Option<i64>,
    pub pings: Option<i64>,
    pub rapier_count: usize,
    pub lane_role: Option<i64>,
    pub comeback: Option<i64>,
    pub throw_amount: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub struct MatchEnrichmentRequest<'a> {
    pub match_id: i64,
    pub guild_id: Option<i64>,
    pub valve_match_id: i64,
    pub duration_seconds: i64,
    pub radiant_score: i64,
    pub dire_score: i64,
    pub game_mode: i64,
    pub enrichment_data: Option<&'a str>,
    pub enrichment_source: Option<&'a str>,
    pub enrichment_confidence: Option<f64>,
    pub participant_updates: &'a [EnrichmentParticipantUpdate],
    pub wrapped_facts: &'a [WrappedEnrichmentFactUpdate],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingBet {
    pub bet_id: i64,
    pub discord_id: i64,
    pub team: TeamSide,
    pub amount: i64,
    pub bet_time: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PendingBetTotals {
    pub radiant: i64,
    pub dire: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LoanState {
    pub outstanding_principal: i64,
    pub outstanding_fee: i64,
}

#[derive(Clone, Debug)]
pub struct MatchRecordingRepository {
    path: PathBuf,
}

impl MatchRecordingRepository {
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

    pub fn add_player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
        rating: f64,
    ) -> Result<(), MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO players (
                 discord_id, guild_id, discord_username, wins, losses,
                 glicko_rating, glicko_rd, glicko_volatility,
                 jopacoin_balance, lowest_balance_ever, updated_at
             ) VALUES (?1, ?2, ?3, 0, 0, ?4, 350.0, 0.06, 3, 3, CURRENT_TIMESTAMP)",
            params![discord_id, Self::normalize_guild_id(guild_id), name, rating],
        )?;
        Ok(())
    }

    pub fn get_player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PlayerRecord>, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT discord_id, discord_username,
                        COALESCE(jopacoin_balance, 0), COALESCE(wins, 0),
                        COALESCE(losses, 0), COALESCE(glicko_rating, 1500.0),
                        COALESCE(glicko_rd, 350.0),
                        COALESCE(glicko_volatility, 0.06)
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                player_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_players_by_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<PlayerRecord>, MatchRecordingRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique = BTreeSet::new();
        unique.extend(discord_ids.iter().copied());
        let connection = self.connection()?;
        let placeholders = std::iter::repeat_n("?", unique.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT discord_id, discord_username,
                    COALESCE(jopacoin_balance, 0), COALESCE(wins, 0),
                    COALESCE(losses, 0), COALESCE(glicko_rating, 1500.0),
                    COALESCE(glicko_rd, 350.0),
                    COALESCE(glicko_volatility, 0.06)
             FROM players WHERE guild_id = ? AND discord_id IN ({placeholders})"
        );
        let parameters =
            std::iter::once(Self::normalize_guild_id(guild_id)).chain(unique.iter().copied());
        let mut statement = connection.prepare(&sql)?;
        let loaded = statement
            .query_map(params_from_iter(parameters), player_from_row)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|player| (player.discord_id, player))
            .collect::<BTreeMap<_, _>>();
        Ok(discord_ids
            .iter()
            .filter_map(|discord_id| loaded.get(discord_id).cloned())
            .collect())
    }

    pub fn set_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        balance: i64,
    ) -> Result<(), MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE players SET jopacoin_balance = ?1,
                    lowest_balance_ever = MIN(COALESCE(lowest_balance_ever, ?1), ?1),
                    updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![balance, discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        if changed == 0 {
            return Err(MatchRecordingRepositoryError::MissingPlayer(discord_id));
        }
        Ok(())
    }

    pub fn add_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<(), MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE players SET
                 jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                 lowest_balance_ever = MIN(
                     COALESCE(lowest_balance_ever, COALESCE(jopacoin_balance, 0) + ?1),
                     COALESCE(jopacoin_balance, 0) + ?1
                 ), updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        if changed == 0 {
            return Err(MatchRecordingRepositoryError::MissingPlayer(discord_id));
        }
        Ok(())
    }

    /// Credit generated income with Python's exact ordering in one
    /// `BEGIN IMMEDIATE` transaction: inspect the live balance, compute the
    /// garnishment split, credit gross, debit bankruptcy withholding, debit
    /// vanity tax, and only then decrement a winner's penalty game.
    ///
    /// Requests are evaluated in order. This matters when a Blood Pact or a
    /// duplicate player causes an earlier movement to change the later live
    /// debt check. Match-win requests may opt into a ledger-backed exact-once
    /// marker; a retry returns the already-persisted balance delta without
    /// crediting again. The source is part of the marker so independent
    /// income phases may safely share one match relation.
    pub fn credit_income_awards_atomic(
        &self,
        guild_id: Option<i64>,
        awards: &[IncomeAwardRequest<'_>],
    ) -> Result<Vec<IncomeAwardReceipt>, MatchRecordingRepositoryError> {
        if awards.is_empty() {
            return Ok(Vec::new());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut receipts = Vec::with_capacity(awards.len());
        for award in awards {
            if award.exact_once
                && let (Some(related_type), Some(related_id)) =
                    (award.related_type, award.related_id)
                && income_award_marker_exists(
                    &transaction,
                    guild_id,
                    award.discord_id,
                    award.source,
                    related_type,
                    related_id,
                )?
            {
                receipts.push(recover_income_award_receipt(
                    &transaction,
                    guild_id,
                    *award,
                )?);
                continue;
            }

            let current_balance = transaction
                .query_row(
                    "SELECT COALESCE(jopacoin_balance,0) FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![award.discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or(MatchRecordingRepositoryError::MissingPlayer(
                    award.discord_id,
                ))?;
            let gross = award.gross;
            if gross <= 0 {
                receipts.push(IncomeAwardReceipt {
                    discord_id: award.discord_id,
                    gross,
                    net: gross,
                    balance_after: current_balance,
                    ..IncomeAwardReceipt::default()
                });
                continue;
            }

            let garnishment_rate = normalized_rate(award.garnishment_rate, 1.0);
            let garnished = if current_balance < 0 {
                truncated_rate(gross, garnishment_rate)
            } else {
                0
            };
            let net_before_penalty = gross.saturating_sub(garnished);
            let penalty_games = transaction
                .query_row(
                    "SELECT COALESCE(penalty_games_remaining,0)
                     FROM bankruptcy_state
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![award.discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or_default();
            let bankruptcy_penalty =
                if award.apply_bankruptcy_penalty && penalty_games > 0 && net_before_penalty > 0 {
                    let kept_rate = normalized_rate(award.bankruptcy_kept_rate, 1.0);
                    truncated_rate(net_before_penalty, 1.0 - kept_rate)
                } else {
                    0
                };
            let vanity_tax = if net_before_penalty > 0 {
                truncated_rate(
                    net_before_penalty,
                    normalized_rate(award.vanity_tax_rate, 0.5),
                )
            } else {
                0
            };
            let metadata = format!(
                "{{\"gross\":{gross},\"garnished\":{garnished},\"bankruptcy_penalty\":{bankruptcy_penalty},\"vanity_tax\":{vanity_tax}}}"
            );

            install_income_ledger_context(
                &transaction,
                *award,
                award.source,
                award.reason,
                &metadata,
            )?;
            transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![gross, award.discord_id, guild_id],
            )?;
            clear_income_ledger_context(&transaction)?;

            if bankruptcy_penalty > 0 {
                let penalty_reason = format!("{} bankruptcy penalty", award.reason);
                install_income_ledger_context(
                    &transaction,
                    *award,
                    award.source,
                    &penalty_reason,
                    &metadata,
                )?;
                transaction.execute(
                    "UPDATE players
                     SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                         updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![bankruptcy_penalty, award.discord_id, guild_id],
                )?;
                clear_income_ledger_context(&transaction)?;
            }
            if vanity_tax > 0 {
                install_income_ledger_context(
                    &transaction,
                    *award,
                    "vanity_tax",
                    "vanity tax on JC profit",
                    &metadata,
                )?;
                transaction.execute(
                    "UPDATE players
                     SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                         updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![vanity_tax, award.discord_id, guild_id],
                )?;
                clear_income_ledger_context(&transaction)?;
            }

            let penalty_games_remaining = if award.decrement_bankruptcy_penalty {
                transaction.execute(
                    "UPDATE bankruptcy_state
                     SET penalty_games_remaining=MAX(
                             COALESCE(penalty_games_remaining,0)-1,0
                         ), updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?1 AND guild_id=?2
                       AND COALESCE(penalty_games_remaining,0)>0",
                    params![award.discord_id, guild_id],
                )?;
                penalty_games.saturating_sub(1).max(0)
            } else {
                penalty_games
            };
            let balance_delta = gross
                .saturating_sub(bankruptcy_penalty)
                .saturating_sub(vanity_tax);
            clear_income_award_compensation_marker(&transaction, guild_id, *award)?;
            receipts.push(IncomeAwardReceipt {
                discord_id: award.discord_id,
                gross,
                garnished,
                bankruptcy_penalty,
                vanity_tax,
                net: net_before_penalty
                    .saturating_sub(bankruptcy_penalty)
                    .saturating_sub(vanity_tax),
                balance_delta,
                balance_after: current_balance.saturating_add(balance_delta),
                penalty_games_remaining,
                applied: true,
            });
        }
        transaction.commit()?;
        Ok(receipts)
    }

    /// Compensate already-applied generated income after a fallible reward
    /// phase.  The debit and its source-scoped retry marker commit together;
    /// the next exact-once award can therefore reapply the source while a
    /// later retry remains idempotent.
    pub fn compensate_income_awards_atomic(
        &self,
        guild_id: Option<i64>,
        compensations: &[IncomeAwardCompensation<'_>],
    ) -> Result<(), MatchRecordingRepositoryError> {
        if compensations.is_empty() {
            return Ok(());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for compensation in compensations {
            if compensation.amount <= 0 {
                continue;
            }
            let metadata = format!("{{\"compensates_source\":\"{}\"}}", compensation.source);
            transaction.execute("DELETE FROM economy_ledger_context", [])?;
            transaction.execute(
                "INSERT INTO economy_ledger_context (
                     id,source,related_type,related_id,reason,metadata
                 ) VALUES (1,'match_bonus_rollback',?1,?2,
                           'match bonus rollback',?3)",
                params![
                    compensation.source,
                    compensation.related_id.to_string(),
                    metadata,
                ],
            )?;
            let changed = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                     lowest_balance_ever=MIN(
                         COALESCE(lowest_balance_ever, COALESCE(jopacoin_balance,0)-?1),
                         COALESCE(jopacoin_balance,0)-?1
                     ),
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![compensation.amount, compensation.discord_id, guild_id],
            )?;
            clear_income_ledger_context(&transaction)?;
            if changed == 0 {
                return Err(MatchRecordingRepositoryError::MissingPlayer(
                    compensation.discord_id,
                ));
            }
        }
        clear_income_ledger_context(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Recover whether the protection-aware Blood Pact phase for one
    /// match-win reward already committed. `Some(0)` is distinct from `None`:
    /// a fully shielded event is still complete and must not reserve pact
    /// capacity again on retry.
    pub fn match_win_blood_pact_skim(
        &self,
        guild_id: Option<i64>,
        match_id: i64,
        discord_id: i64,
    ) -> Result<Option<i64>, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT applied FROM hostile_loss_events
                 WHERE guild_id=?1 AND victim_id=?2 AND kind='blood_pact'
                   AND event_key=?3",
                params![
                    Self::normalize_guild_id(guild_id),
                    discord_id,
                    format!("match-win-blood-pact:{match_id}:{discord_id}")
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Recover the independently credited Communion component for one
    /// exact-once match-win award.  The base income receipt deliberately
    /// excludes this row so callers can reconstruct the same component split
    /// on a retry without also folding a Blood-Pact debit into the base award.
    pub fn match_win_communion_bonus(
        &self,
        guild_id: Option<i64>,
        match_id: i64,
        discord_id: i64,
    ) -> Result<i64, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(SUM(delta),0)
                 FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_type='player' AND account_id=?2
                   AND source='manashop_buff'
                   AND related_type='match_win_bonus' AND related_id=?3",
                params![
                    Self::normalize_guild_id(guild_id),
                    discord_id,
                    match_id.to_string()
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn place_bet_atomic(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        team: TeamSide,
        amount: i64,
        bet_time: i64,
    ) -> Result<i64, MatchRecordingRepositoryError> {
        if amount <= 0 {
            return Err(MatchRecordingRepositoryError::InsufficientBalance { discord_id, amount });
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE players SET jopacoin_balance = jopacoin_balance - ?1,
                    lowest_balance_ever = MIN(
                        COALESCE(lowest_balance_ever, jopacoin_balance - ?1),
                        jopacoin_balance - ?1
                    ), updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) >= ?1",
            params![amount, discord_id, guild_id],
        )?;
        if changed == 0 {
            return Err(MatchRecordingRepositoryError::InsufficientBalance { discord_id, amount });
        }
        transaction.execute(
            "INSERT INTO bets (
                 guild_id, match_id, discord_id, team_bet_on, amount, bet_time
             ) VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
            params![guild_id, discord_id, team.name(), amount, bet_time],
        )?;
        let bet_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(bet_id)
    }

    pub fn pending_bet_totals(
        &self,
        guild_id: Option<i64>,
        since: i64,
    ) -> Result<PendingBetTotals, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT
                     COALESCE(SUM(CASE WHEN team_bet_on = 'radiant' THEN amount ELSE 0 END), 0),
                     COALESCE(SUM(CASE WHEN team_bet_on = 'dire' THEN amount ELSE 0 END), 0)
                 FROM bets
                 WHERE guild_id = ?1 AND match_id IS NULL AND bet_time >= ?2",
                params![Self::normalize_guild_id(guild_id), since],
                |row| {
                    Ok(PendingBetTotals {
                        radiant: row.get(0)?,
                        dire: row.get(1)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn get_pending_bet(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        since: i64,
    ) -> Result<Option<PendingBet>, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT bet_id, discord_id, team_bet_on, amount, bet_time
                 FROM bets
                 WHERE guild_id = ?1 AND discord_id = ?2
                   AND match_id IS NULL AND bet_time >= ?3
                 ORDER BY bet_id DESC LIMIT 1",
                params![Self::normalize_guild_id(guild_id), discord_id, since],
                |row| {
                    let team_name = row.get::<_, String>(2)?;
                    Ok(PendingBet {
                        bet_id: row.get(0)?,
                        discord_id: row.get(1)?,
                        team: TeamSide::parse(&team_name).unwrap_or(TeamSide::Radiant),
                        amount: row.get(3)?,
                        bet_time: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// WARNING — test-double only, never wire to production. The flat
    /// `principal / 5` fee diverges from the real loan policy
    /// (`loan_repository::LoanRepository`), which is what `cama-runtime`
    /// uses. Only `MatchRecordingService` tests reach this path.
    pub fn issue_loan_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        principal: i64,
        now: i64,
    ) -> Result<LoanState, MatchRecordingRepositoryError> {
        if principal <= 0 {
            return Err(MatchRecordingRepositoryError::InvalidLoanAmount);
        }
        let fee = principal / 5;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT COALESCE(outstanding_principal, 0)
                 FROM loan_state WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or_default();
        if existing > 0 {
            return Err(MatchRecordingRepositoryError::OutstandingLoan(discord_id));
        }
        let changed = transaction.execute(
            "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                    updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![principal, discord_id, guild_id],
        )?;
        if changed == 0 {
            return Err(MatchRecordingRepositoryError::MissingPlayer(discord_id));
        }
        transaction.execute(
            "INSERT INTO loan_state (
                 discord_id, guild_id, last_loan_at, total_loans_taken,
                 total_fees_paid, negative_loans_taken,
                 outstanding_principal, outstanding_fee, updated_at
             ) VALUES (?1, ?2, ?3, 1, 0, 0, ?4, ?5, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 last_loan_at = excluded.last_loan_at,
                 total_loans_taken = loan_state.total_loans_taken + 1,
                 outstanding_principal = excluded.outstanding_principal,
                 outstanding_fee = excluded.outstanding_fee,
                 updated_at = CURRENT_TIMESTAMP",
            params![discord_id, guild_id, now, principal, fee],
        )?;
        transaction.commit()?;
        Ok(LoanState {
            outstanding_principal: principal,
            outstanding_fee: fee,
        })
    }

    pub fn loan_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<LoanState, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(outstanding_principal, 0),
                        COALESCE(outstanding_fee, 0)
                 FROM loan_state WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(LoanState {
                        outstanding_principal: row.get(0)?,
                        outstanding_fee: row.get(1)?,
                    })
                },
            )
            .optional()
            .map(|state| state.unwrap_or_default())
            .map_err(Into::into)
    }

    pub fn nonprofit_total(
        &self,
        guild_id: Option<i64>,
    ) -> Result<i64, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(total_collected, 0)
                 FROM nonprofit_fund WHERE guild_id = ?1",
                [Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.unwrap_or_default())
            .map_err(Into::into)
    }

    /// WARNING — test-double only, never wire to production. The rating
    /// update below is a fixed ±32 stub, not Glicko-2; the production match
    /// recording path is `core_repositories::MatchRepository::
    /// record_match_core_atomic`. Only `MatchRecordingService` (itself
    /// constructed exclusively in tests) reaches this function.
    pub fn record_match_atomic(
        &self,
        request: MatchRecordRequest<'_>,
    ) -> Result<MatchFinalization, MatchRecordingRepositoryError> {
        validate_teams(&request)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let winning_ids = if request.winning_team == TeamSide::Radiant {
            request.radiant_ids.to_vec()
        } else {
            request.dire_ids.to_vec()
        };
        let losing_ids = if request.winning_team == TeamSide::Radiant {
            request.dire_ids.to_vec()
        } else {
            request.radiant_ids.to_vec()
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(pending_match_id) = request.pending_match_id {
            let existing_match_id = transaction
                .query_row(
                    "SELECT match_id FROM matches
                     WHERE guild_id = ?1 AND pending_match_id = ?2",
                    params![guild_id, pending_match_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(match_id) = existing_match_id {
                transaction.commit()?;
                return Ok(MatchFinalization {
                    match_id,
                    winning_team: request.winning_team,
                    winning_player_ids: winning_ids,
                    losing_player_ids: losing_ids,
                    excluded_player_ids: request.all_excluded_ids.to_vec(),
                    updated_count: 0,
                    bet_distributions: BetDistributions::default(),
                    jc_changes: BTreeMap::new(),
                    loan_repayments: Vec::new(),
                });
            }
        }

        transaction.execute(
            "INSERT INTO matches (
                 guild_id, team1_players, team2_players, winning_team,
                 lobby_type, balancing_rating_system, betting_mode,
                 win_reward_jc, pending_match_id, match_date
             ) VALUES (?1, ?2, ?3, ?4, 'shuffle', 'glicko', 'house', ?5,
                       ?6, CURRENT_TIMESTAMP)",
            params![
                guild_id,
                encode_ids(request.radiant_ids),
                encode_ids(request.dire_ids),
                request.winning_team.number(),
                request.win_reward,
                request.pending_match_id
            ],
        )?;
        let match_id = transaction.last_insert_rowid();
        let mut jc_changes = BTreeMap::<i64, JcChange>::new();

        for (team, ids) in [
            (TeamSide::Radiant, request.radiant_ids),
            (TeamSide::Dire, request.dire_ids),
        ] {
            let won = team == request.winning_team;
            for discord_id in ids {
                let before = player_for_update(&transaction, *discord_id, guild_id)?;
                let rating_delta = if request.update_ratings {
                    if won { 32.0 } else { -32.0 }
                } else {
                    0.0
                };
                let after_rating = before.glicko_rating + rating_delta;
                transaction.execute(
                    "UPDATE players SET
                         wins = COALESCE(wins, 0) + ?1,
                         losses = COALESCE(losses, 0) + ?2,
                         glicko_rating = ?3,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?4 AND guild_id = ?5",
                    params![
                        i64::from(won),
                        i64::from(!won),
                        after_rating,
                        discord_id,
                        guild_id
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO match_participants (
                         match_id, discord_id, guild_id, team_number, won, side
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        match_id,
                        discord_id,
                        guild_id,
                        team.number(),
                        won,
                        team.name()
                    ],
                )?;
                if request.update_ratings {
                    transaction.execute(
                        "INSERT INTO rating_history (
                             discord_id, guild_id, rating, rating_before,
                             rd_before, rd_after, volatility_before,
                             volatility_after, team_number, won, match_id
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?6, ?7, ?8, ?9)",
                        params![
                            discord_id,
                            guild_id,
                            after_rating,
                            before.glicko_rating,
                            before.glicko_rd,
                            before.glicko_volatility,
                            team.number(),
                            won,
                            match_id
                        ],
                    )?;
                }
            }
        }

        update_pairings(
            &transaction,
            guild_id,
            match_id,
            request.radiant_ids,
            request.dire_ids,
            request.winning_team,
        )?;
        update_exclusion_counts(
            &transaction,
            guild_id,
            request.exclusion_decay_ids,
            request.full_exclusion_increment_ids,
            request.half_exclusion_increment_ids,
        )?;

        let bet_distributions = settle_house_bets(
            &transaction,
            guild_id,
            match_id,
            request.pending_bet_since,
            request.winning_team,
            &mut jc_changes,
        )?;
        for discord_id in &winning_ids {
            credit_reward(
                &transaction,
                *discord_id,
                guild_id,
                request.win_reward,
                &mut jc_changes,
            )?;
        }
        for discord_id in &losing_ids {
            credit_reward(
                &transaction,
                *discord_id,
                guild_id,
                request.participation_reward,
                &mut jc_changes,
            )?;
        }
        for discord_id in request.rewarded_excluded_ids {
            credit_reward(
                &transaction,
                *discord_id,
                guild_id,
                request.exclusion_reward,
                &mut jc_changes,
            )?;
        }
        let participant_order = winning_ids
            .iter()
            .chain(losing_ids.iter())
            .copied()
            .collect::<Vec<_>>();
        let loan_repayments = repay_participant_loans(&transaction, guild_id, &participant_order)?;

        transaction.commit()?;
        Ok(MatchFinalization {
            match_id,
            winning_team: request.winning_team,
            winning_player_ids: winning_ids,
            losing_player_ids: losing_ids,
            excluded_player_ids: request.all_excluded_ids.to_vec(),
            updated_count: request.radiant_ids.len() + request.dire_ids.len(),
            bet_distributions,
            jc_changes,
            loan_repayments,
        })
    }

    pub fn get_match(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<StoredMatch>, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT match_id, team1_players, team2_players, winning_team
                 FROM matches WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    let radiant = row.get::<_, String>(1)?;
                    let dire = row.get::<_, String>(2)?;
                    Ok(StoredMatch {
                        match_id: row.get(0)?,
                        radiant_ids: decode_ids(&radiant),
                        dire_ids: decode_ids(&dire),
                        winning_team: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_participants(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<ParticipantRecord>, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, team_number, won, side
             FROM match_participants
             WHERE match_id = ?1 AND guild_id = ?2 ORDER BY discord_id",
        )?;
        statement
            .query_map(
                params![match_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(ParticipantRecord {
                        discord_id: row.get(0)?,
                        team_number: row.get(1)?,
                        won: row.get(2)?,
                        side: row.get(3)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn rating_history(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<RatingHistoryRecord>, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, rating, rating_before, won
             FROM rating_history
             WHERE match_id = ?1 AND guild_id = ?2 ORDER BY discord_id",
        )?;
        statement
            .query_map(
                params![match_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(RatingHistoryRecord {
                        discord_id: row.get(0)?,
                        rating: row.get(1)?,
                        rating_before: row.get(2)?,
                        won: row.get(3)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Apply match-level metadata and every supplied participant stat row in
    /// one immediate transaction, preventing half-enriched matches.
    pub fn apply_enrichment_atomic(
        &self,
        request: MatchEnrichmentRequest<'_>,
    ) -> Result<usize, MatchRecordingRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE matches
             SET valve_match_id = ?1, duration_seconds = ?2,
                 radiant_score = ?3, dire_score = ?4, game_mode = ?5,
                 enrichment_data = ?6, enrichment_source = ?7,
                 enrichment_confidence = ?8
             WHERE match_id = ?9 AND guild_id = ?10",
            params![
                request.valve_match_id,
                request.duration_seconds,
                request.radiant_score,
                request.dire_score,
                request.game_mode,
                request.enrichment_data,
                request.enrichment_source,
                request.enrichment_confidence,
                request.match_id,
                guild_id
            ],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Ok(0);
        }

        replace_match_bans(&transaction, request.match_id, request.enrichment_data)?;
        transaction.execute(
            "DELETE FROM wrapped_enrichment_facts
              WHERE guild_id = ?1 AND match_id = ?2",
            params![guild_id, request.match_id],
        )?;
        for fact in request.wrapped_facts {
            transaction.execute(
                "INSERT INTO wrapped_enrichment_facts (
                     guild_id, match_id, discord_id, actions_per_min,
                     courier_kills, pings, rapier_count, lane_role, comeback, throw
                 )
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
                  WHERE EXISTS (
                     SELECT 1 FROM match_participants
                      WHERE guild_id = ?1 AND match_id = ?2 AND discord_id = ?3
                  )
                 ON CONFLICT(guild_id, match_id, discord_id) DO UPDATE SET
                     actions_per_min = excluded.actions_per_min,
                     courier_kills = excluded.courier_kills,
                     pings = excluded.pings,
                     rapier_count = excluded.rapier_count,
                     lane_role = excluded.lane_role,
                     comeback = excluded.comeback,
                     throw = excluded.throw",
                params![
                    guild_id,
                    request.match_id,
                    fact.discord_id,
                    fact.actions_per_min,
                    fact.courier_kills,
                    fact.pings,
                    fact.rapier_count as i64,
                    fact.lane_role,
                    fact.comeback,
                    fact.throw_amount,
                ],
            )?;
        }

        let mut updated = 0;
        for participant in request.participant_updates {
            updated += transaction.execute(
                "UPDATE match_participants
                 SET hero_id = ?1, kills = ?2, deaths = ?3, assists = ?4,
                     gpm = ?5, xpm = ?6, hero_damage = ?7,
                     tower_damage = ?8, last_hits = ?9, denies = ?10,
                     net_worth = ?11, hero_healing = ?12, lane_role = ?13,
                     lane_efficiency = ?14, towers_killed = ?15,
                     roshans_killed = ?16, teamfight_participation = ?17,
                     obs_placed = ?18, sen_placed = ?19, camps_stacked = ?20,
                     rune_pickups = ?21, firstblood_claimed = ?22, stuns = ?23,
                     fantasy_points = ?24, gold_at_10 = ?25
                 WHERE match_id = ?26 AND discord_id = ?27 AND guild_id = ?28",
                params![
                    participant.hero_id,
                    participant.kills,
                    participant.deaths,
                    participant.assists,
                    participant.gpm,
                    participant.xpm,
                    participant.hero_damage,
                    participant.tower_damage,
                    participant.last_hits,
                    participant.denies,
                    participant.net_worth,
                    participant.hero_healing,
                    participant.lane_role,
                    participant.lane_efficiency,
                    participant.towers_killed,
                    participant.roshans_killed,
                    participant.teamfight_participation,
                    participant.obs_placed,
                    participant.sen_placed,
                    participant.camps_stacked,
                    participant.rune_pickups,
                    participant.firstblood_claimed,
                    participant.stuns,
                    participant.fantasy_points,
                    participant.gold_at_10,
                    request.match_id,
                    participant.discord_id,
                    guild_id
                ],
            )?;
        }
        let (participant_count, fantasy_count) = transaction.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN fantasy_points IS NOT NULL THEN 1 ELSE 0 END)
               FROM match_participants
              WHERE match_id = ?1 AND guild_id = ?2",
            params![request.match_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                ))
            },
        )?;
        if participant_count == 10 && fantasy_count == 10 {
            transaction.execute(
                "INSERT INTO openskill_replay_jobs (
                     guild_id, reason, requested_at, last_error
                 ) VALUES (?1, ?2, CURRENT_TIMESTAMP, NULL)
                 ON CONFLICT(guild_id) DO UPDATE SET
                     reason = excluded.reason,
                     requested_at = excluded.requested_at,
                     last_error = NULL",
                params![guild_id, format!("match_enrichment:{}", request.match_id)],
            )?;
        }
        transaction.commit()?;
        Ok(updated)
    }

    pub fn match_count(&self, guild_id: Option<i64>) -> Result<i64, MatchRecordingRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COUNT(*) FROM matches WHERE guild_id = ?1",
                [Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}

fn replace_match_bans(
    transaction: &rusqlite::Transaction<'_>,
    match_id: i64,
    enrichment_data: Option<&str>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM match_bans WHERE match_id = ?1", [match_id])?;
    let Some(raw) = enrichment_data else {
        return Ok(());
    };
    let Ok(payload) = serde_json::from_str::<JsonValue>(raw) else {
        return Ok(());
    };
    let Some(entries) = payload.get("picks_bans").and_then(JsonValue::as_array) else {
        return Ok(());
    };
    for (ban_index, entry) in entries.iter().enumerate() {
        let Some(fields) = entry.as_object() else {
            continue;
        };
        let is_ban = fields.get("is_pick").is_some_and(json_false_or_zero);
        if !is_ban {
            continue;
        }
        let Some(team) = fields.get("team").and_then(json_ban_integer) else {
            continue;
        };
        let Some(hero_id) = fields.get("hero_id").and_then(json_ban_integer) else {
            continue;
        };
        if !matches!(team, 0 | 1) || hero_id <= 0 {
            continue;
        }
        transaction.execute(
            "INSERT INTO match_bans(match_id,ban_index,team,hero_id)
             VALUES (?1,?2,?3,?4)",
            params![
                match_id,
                i64::try_from(ban_index).unwrap_or(i64::MAX),
                team,
                hero_id
            ],
        )?;
    }
    Ok(())
}

fn json_false_or_zero(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Bool(false))
        || matches!(value, JsonValue::Number(number) if number.as_f64() == Some(0.0))
}

fn json_ban_integer(value: &JsonValue) -> Option<i64> {
    (!value.is_boolean())
        .then(|| coerce_i64_like_python(value))
        .flatten()
}

fn player_from_row(row: &rusqlite::Row<'_>) -> Result<PlayerRecord, rusqlite::Error> {
    Ok(PlayerRecord {
        discord_id: row.get(0)?,
        name: row.get(1)?,
        balance: row.get(2)?,
        wins: row.get(3)?,
        losses: row.get(4)?,
        glicko_rating: row.get(5)?,
        glicko_rd: row.get(6)?,
        glicko_volatility: row.get(7)?,
    })
}

fn player_for_update(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<PlayerRecord, MatchRecordingRepositoryError> {
    connection
        .query_row(
            "SELECT discord_id, discord_username,
                    COALESCE(jopacoin_balance, 0), COALESCE(wins, 0),
                    COALESCE(losses, 0), COALESCE(glicko_rating, 1500.0),
                    COALESCE(glicko_rd, 350.0),
                    COALESCE(glicko_volatility, 0.06)
             FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            player_from_row,
        )
        .optional()?
        .ok_or(MatchRecordingRepositoryError::MissingPlayer(discord_id))
}

fn validate_teams(request: &MatchRecordRequest<'_>) -> Result<(), MatchRecordingRepositoryError> {
    let radiant = request.radiant_ids.iter().copied().collect::<BTreeSet<_>>();
    let dire = request.dire_ids.iter().copied().collect::<BTreeSet<_>>();
    if radiant.len() != request.radiant_ids.len() || dire.len() != request.dire_ids.len() {
        return Err(MatchRecordingRepositoryError::InvalidTeams(
            "duplicate player within a team",
        ));
    }
    if !radiant.is_disjoint(&dire) {
        return Err(MatchRecordingRepositoryError::InvalidTeams(
            "player appears on both teams",
        ));
    }
    let participants = radiant.union(&dire).copied().collect::<BTreeSet<_>>();
    if request
        .all_excluded_ids
        .iter()
        .any(|discord_id| participants.contains(discord_id))
    {
        return Err(MatchRecordingRepositoryError::InvalidTeams(
            "excluded player appears in a match team",
        ));
    }
    Ok(())
}

fn canonical_pair(first: i64, second: i64) -> (i64, i64) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

fn update_pairings(
    transaction: &rusqlite::Transaction<'_>,
    guild_id: i64,
    match_id: i64,
    radiant_ids: &[i64],
    dire_ids: &[i64],
    winner: TeamSide,
) -> Result<(), rusqlite::Error> {
    for (team, ids) in [(TeamSide::Radiant, radiant_ids), (TeamSide::Dire, dire_ids)] {
        let won = i64::from(team == winner);
        for (index, &first) in ids.iter().enumerate() {
            for &second in &ids[index + 1..] {
                let (player1, player2) = canonical_pair(first, second);
                transaction.execute(
                    "INSERT INTO player_pairings (
                         guild_id, player1_id, player2_id, games_together,
                         wins_together, last_match_id
                     ) VALUES (?1, ?2, ?3, 1, ?4, ?5)
                     ON CONFLICT(guild_id, player1_id, player2_id) DO UPDATE SET
                         games_together = games_together + 1,
                         wins_together = wins_together + excluded.wins_together,
                         last_match_id = excluded.last_match_id,
                         updated_at = CURRENT_TIMESTAMP",
                    params![guild_id, player1, player2, won, match_id],
                )?;
            }
        }
    }
    for &radiant in radiant_ids {
        for &dire in dire_ids {
            let (player1, player2) = canonical_pair(radiant, dire);
            let player1_is_radiant = player1 == radiant;
            let player1_won = if player1_is_radiant {
                winner == TeamSide::Radiant
            } else {
                winner == TeamSide::Dire
            };
            transaction.execute(
                "INSERT INTO player_pairings (
                     guild_id, player1_id, player2_id, games_against,
                     player1_wins_against, last_match_id
                 ) VALUES (?1, ?2, ?3, 1, ?4, ?5)
                 ON CONFLICT(guild_id, player1_id, player2_id) DO UPDATE SET
                     games_against = games_against + 1,
                     player1_wins_against = player1_wins_against
                         + excluded.player1_wins_against,
                     last_match_id = excluded.last_match_id,
                     updated_at = CURRENT_TIMESTAMP",
                params![guild_id, player1, player2, i64::from(player1_won), match_id],
            )?;
        }
    }
    Ok(())
}

fn update_exclusion_counts(
    transaction: &rusqlite::Transaction<'_>,
    guild_id: i64,
    decay_ids: &[i64],
    full_increment_ids: &[i64],
    half_increment_ids: &[i64],
) -> Result<(), rusqlite::Error> {
    for discord_id in decay_ids {
        transaction.execute(
            "UPDATE players
             SET exclusion_count = MAX(COALESCE(exclusion_count, 0) - 1, 0),
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
    }
    for discord_id in full_increment_ids {
        transaction.execute(
            "UPDATE players
             SET exclusion_count = COALESCE(exclusion_count, 0) + 6,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
    }
    for discord_id in half_increment_ids {
        transaction.execute(
            "UPDATE players
             SET exclusion_count = COALESCE(exclusion_count, 0) + 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
    }
    Ok(())
}

fn settle_house_bets(
    connection: &Connection,
    guild_id: i64,
    match_id: i64,
    since: i64,
    winner: TeamSide,
    jc_changes: &mut BTreeMap<i64, JcChange>,
) -> Result<BetDistributions, MatchRecordingRepositoryError> {
    let bets = {
        let mut statement = connection.prepare(
            "SELECT bet_id, discord_id, team_bet_on, amount, bet_time
             FROM bets
             WHERE guild_id = ?1 AND match_id IS NULL AND bet_time >= ?2
             ORDER BY bet_id",
        )?;
        statement
            .query_map(params![guild_id, since], |row| {
                let team_name = row.get::<_, String>(2)?;
                Ok(PendingBet {
                    bet_id: row.get(0)?,
                    discord_id: row.get(1)?,
                    team: TeamSide::parse(&team_name).unwrap_or(TeamSide::Radiant),
                    amount: row.get(3)?,
                    bet_time: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut distributions = BetDistributions::default();
    for bet in bets {
        connection.execute(
            "UPDATE bets SET match_id = ?1 WHERE bet_id = ?2 AND match_id IS NULL",
            params![match_id, bet.bet_id],
        )?;
        if bet.team == winner {
            let gross_credit = bet.amount * 2;
            let changed = connection.execute(
                "UPDATE players SET jopacoin_balance = jopacoin_balance + ?1,
                        updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![gross_credit, bet.discord_id, guild_id],
            )?;
            if changed == 0 {
                return Err(MatchRecordingRepositoryError::MissingPlayer(bet.discord_id));
            }
            jc_changes.entry(bet.discord_id).or_default().bet += bet.amount;
            distributions.winners.push(BetResult {
                discord_id: bet.discord_id,
                team: bet.team,
                amount: bet.amount,
                gross_credit,
            });
        } else {
            jc_changes.entry(bet.discord_id).or_default().bet -= bet.amount;
            distributions.losers.push(BetResult {
                discord_id: bet.discord_id,
                team: bet.team,
                amount: bet.amount,
                gross_credit: 0,
            });
        }
    }
    Ok(distributions)
}

fn credit_reward(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    amount: i64,
    jc_changes: &mut BTreeMap<i64, JcChange>,
) -> Result<(), MatchRecordingRepositoryError> {
    let changed = connection.execute(
        "UPDATE players SET jopacoin_balance = jopacoin_balance + ?1,
                updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3",
        params![amount, discord_id, guild_id],
    )?;
    if changed == 0 {
        return Err(MatchRecordingRepositoryError::MissingPlayer(discord_id));
    }
    jc_changes.entry(discord_id).or_default().payout += amount;
    Ok(())
}

fn repay_participant_loans(
    connection: &Connection,
    guild_id: i64,
    participant_ids: &[i64],
) -> Result<Vec<LoanRepayment>, MatchRecordingRepositoryError> {
    let mut repayments = Vec::new();
    let mut seen = BTreeSet::new();
    let mut nonprofit_total = connection
        .query_row(
            "SELECT COALESCE(total_collected, 0)
             FROM nonprofit_fund WHERE guild_id = ?1",
            [guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or_default();
    for discord_id in participant_ids {
        if !seen.insert(*discord_id) {
            continue;
        }
        let state = connection
            .query_row(
                "SELECT COALESCE(outstanding_principal, 0),
                        COALESCE(outstanding_fee, 0)
                 FROM loan_state WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((principal, fee)) = state else {
            continue;
        };
        if principal <= 0 {
            continue;
        }
        let total = principal + fee;
        let balance_before = connection.query_row(
            "SELECT COALESCE(jopacoin_balance, 0) FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )?;
        let new_balance = balance_before - total;
        connection.execute(
            "UPDATE players SET jopacoin_balance = ?1,
                    lowest_balance_ever = MIN(COALESCE(lowest_balance_ever, ?1), ?1),
                    updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![new_balance, discord_id, guild_id],
        )?;
        connection.execute(
            "UPDATE loan_state SET outstanding_principal = 0,
                    outstanding_fee = 0,
                    total_fees_paid = COALESCE(total_fees_paid, 0) + ?1,
                    updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![fee, discord_id, guild_id],
        )?;
        nonprofit_total += fee;
        repayments.push(LoanRepayment {
            player_id: *discord_id,
            principal,
            fee,
            total_repaid: total,
            balance_before,
            new_balance,
            nonprofit_total,
        });
    }
    if !repayments.is_empty() {
        connection.execute(
            "INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
             VALUES (?1, ?2, CURRENT_TIMESTAMP)
             ON CONFLICT(guild_id) DO UPDATE SET
                 total_collected = excluded.total_collected,
                 updated_at = CURRENT_TIMESTAMP",
            params![guild_id, nonprofit_total],
        )?;
    }
    Ok(repayments)
}

fn normalized_rate(rate: f64, maximum: f64) -> f64 {
    if rate.is_nan() {
        maximum
    } else {
        rate.clamp(0.0, maximum)
    }
}

fn truncated_rate(amount: i64, rate: f64) -> i64 {
    (amount as f64 * rate) as i64
}

fn install_income_ledger_context(
    transaction: &rusqlite::Transaction<'_>,
    award: IncomeAwardRequest<'_>,
    source: &str,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id,source,related_type,related_id,reason,metadata
         ) VALUES (1,?1,?2,?3,?4,?5)",
        params![
            source,
            award.related_type,
            award.related_id.map(|value| value.to_string()),
            reason,
            metadata,
        ],
    )?;
    Ok(())
}

fn clear_income_ledger_context(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn income_award_marker_exists(
    transaction: &rusqlite::Transaction<'_>,
    guild_id: i64,
    discord_id: i64,
    source: &str,
    related_type: &str,
    related_id: i64,
) -> Result<bool, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT 1 FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_type='player' AND account_id=?2
               AND source=?3 AND related_type=?4 AND related_id=?5
               AND NOT EXISTS (
                   SELECT 1 FROM economy_ledger_entries compensation
                   WHERE compensation.guild_id=?1
                     AND compensation.account_type='player'
                     AND compensation.account_id=?2
                     AND compensation.source='match_bonus_rollback'
                     AND compensation.related_type=?3
                     AND compensation.related_id=?5
               )
             LIMIT 1",
            params![
                guild_id,
                discord_id,
                source,
                related_type,
                related_id.to_string()
            ],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn clear_income_award_compensation_marker(
    transaction: &rusqlite::Transaction<'_>,
    guild_id: i64,
    award: IncomeAwardRequest<'_>,
) -> Result<(), rusqlite::Error> {
    let Some(related_id) = award.related_id else {
        return Ok(());
    };
    transaction.execute(
        "DELETE FROM economy_ledger_entries
         WHERE guild_id=?1 AND account_type='player' AND account_id=?2
           AND source='match_bonus_rollback'
           AND related_type=?3 AND related_id=?4",
        params![
            guild_id,
            award.discord_id,
            award.source,
            related_id.to_string()
        ],
    )?;
    Ok(())
}

fn recover_income_award_receipt(
    transaction: &rusqlite::Transaction<'_>,
    guild_id: i64,
    award: IncomeAwardRequest<'_>,
) -> Result<IncomeAwardReceipt, MatchRecordingRepositoryError> {
    let related_type = award
        .related_type
        .expect("exact-once award has a related type");
    let related_id = award.related_id.expect("exact-once award has a related ID");
    // Communion and protection-aware Blood Pact rows may share the durable
    // match relation, but are recovered independently by the runtime.
    let (source_delta, metadata) = transaction.query_row(
        "SELECT COALESCE(SUM(delta),0),
                MAX(CASE WHEN source=?5 THEN metadata END)
         FROM economy_ledger_entries
         WHERE guild_id=?1 AND account_type='player' AND account_id=?2
           AND related_type=?3 AND related_id=?4
           AND source=?5",
        params![
            guild_id,
            award.discord_id,
            related_type,
            related_id.to_string(),
            award.source,
        ],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    let metadata = metadata
        .as_deref()
        .and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok());
    let component = |name: &str| {
        metadata
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(JsonValue::as_i64)
            .unwrap_or_default()
    };
    let gross = component("gross");
    let garnished = component("garnished");
    let bankruptcy_penalty = component("bankruptcy_penalty");
    let vanity_tax = component("vanity_tax");
    // Bankruptcy debits retain the base source. Vanity-tax rows use the
    // shared `vanity_tax` source, so recover that deduction from the base
    // row's immutable metadata rather than summing another award's tax.
    let balance_delta = source_delta.saturating_sub(vanity_tax);
    let balance_after = transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![award.discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(MatchRecordingRepositoryError::MissingPlayer(
            award.discord_id,
        ))?;
    let penalty_games_remaining = transaction
        .query_row(
            "SELECT COALESCE(penalty_games_remaining,0) FROM bankruptcy_state
             WHERE discord_id=?1 AND guild_id=?2",
            params![award.discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default();
    Ok(IncomeAwardReceipt {
        discord_id: award.discord_id,
        gross,
        garnished,
        bankruptcy_penalty,
        vanity_tax,
        net: gross
            .saturating_sub(garnished)
            .saturating_sub(bankruptcy_penalty)
            .saturating_sub(vanity_tax),
        balance_delta,
        balance_after,
        penalty_games_remaining,
        applied: false,
    })
}

fn encode_ids(ids: &[i64]) -> String {
    format!(
        "[{}]",
        ids.iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn decode_ids(raw: &str) -> Vec<i64> {
    raw.trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .map(|body| {
            body.split(',')
                .filter_map(|value| value.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "match_recording/tests.rs"]
mod tests;
