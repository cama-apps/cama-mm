//! Existing-schema persistence for after-the-fact match-result corrections.
//!
//! Python remains the schema, migration, orchestration, and live-runtime
//! authority during parity. This adapter opens only an existing database via
//! [`crate::open_runtime_connection`]. It intentionally preserves Python's
//! recovery seams: ownership, winner-bonus moves, the core result flip, bet
//! settlement, and the OpenSkill replay worker are separate durable steps.
//! There is no claim here that the complete correction pipeline is one SQLite
//! transaction.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use cama_domain::openskill::{
    CamaOpenSkillSystem, Rating as OpenSkillRating, WeightedPlayer, WinningTeam,
};
use cama_domain::rating::{
    CamaRatingSystem, MatchUpdateOptions, NEW_PLAYER_MMR_DISCOUNT, RecordedValue, TeamPlayer,
    cap_glicko_rd, recorded_streak_rate, recorded_streak_threshold,
};

use crate::open_runtime_connection;
use crate::rating_history_repository::RatingHistoryRepository;

pub const LEGACY_WIN_REWARD_JC: i64 = 4;
pub const OPENSKILL_REPLAY_ALGORITHM_VERSION: i64 = 5;
const RECENT_OUTCOME_LIMIT: usize = 20;

#[must_use]
pub const fn recorded_win_reward_jc(stored: Option<i64>) -> i64 {
    match stored {
        Some(value) => value,
        None => LEGACY_WIN_REWARD_JC,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchSide {
    Radiant,
    Dire,
}

impl MatchSide {
    #[must_use]
    pub const fn number(self) -> i64 {
        match self {
            Self::Radiant => 1,
            Self::Dire => 2,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }

    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Radiant => Self::Dire,
            Self::Dire => Self::Radiant,
        }
    }
}

impl TryFrom<i64> for MatchSide {
    type Error = MatchCorrectionError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Radiant),
            2 => Ok(Self::Dire),
            value => Err(MatchCorrectionError::InvalidWinningTeam(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimState {
    Pending,
    CoreApplied,
    Already,
}

impl ClaimState {
    fn parse(value: &str) -> Result<Self, MatchCorrectionError> {
        match value {
            "pending" => Ok(Self::Pending),
            "core_applied" => Ok(Self::CoreApplied),
            value => Err(MatchCorrectionError::InvalidClaimState(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrectionClaim {
    pub match_id: i64,
    pub guild_id: i64,
    pub old_winning_team: MatchSide,
    pub new_winning_team: MatchSide,
    pub state: ClaimState,
    pub correction_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RatingCorrection {
    pub discord_id: i64,
    pub rating: f64,
    pub rd: f64,
    pub volatility: f64,
    pub won: bool,
    pub os_mu: Option<f64>,
    pub os_sigma: Option<f64>,
    pub fantasy_weight: Option<f64>,
    pub streak_length: Option<i64>,
    pub streak_multiplier: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpenSkillReplayUpdate {
    pub match_id: i64,
    pub discord_id: i64,
    pub mu: f64,
    pub sigma: f64,
    pub streak_length: i64,
    pub streak_multiplier: f64,
    pub is_live_rating: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenSkillReplaySummary {
    pub matches_processed: usize,
    pub matches_with_fantasy: usize,
    pub matches_equal_weight: usize,
    pub players_updated: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct CoreCorrectionRequest<'a> {
    pub match_id: i64,
    pub guild_id: Option<i64>,
    pub old_winning_team: MatchSide,
    pub new_winning_team: MatchSide,
    pub corrected_by: Option<i64>,
    pub owner_token: &'a str,
    pub rating_updates: &'a [RatingCorrection],
    pub openskill_algorithm_version: i64,
    pub openskill_algorithm_fingerprint: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct BetCorrectionOptions<'a> {
    pub pool_mode: bool,
    pub house_payout_multiplier: f64,
    pub vanity_tax_rate: f64,
    pub vanity_taxable_ids: &'a BTreeSet<i64>,
    pub low_priority_tax_rate: f64,
    pub low_priority_taxable_ids: &'a BTreeSet<i64>,
}

impl<'a> BetCorrectionOptions<'a> {
    #[must_use]
    pub const fn pool(
        vanity_tax_rate: f64,
        vanity_taxable_ids: &'a BTreeSet<i64>,
        low_priority_tax_rate: f64,
        low_priority_taxable_ids: &'a BTreeSet<i64>,
    ) -> Self {
        Self {
            pool_mode: true,
            house_payout_multiplier: 1.0,
            vanity_tax_rate,
            vanity_taxable_ids,
            low_priority_tax_rate,
            low_priority_taxable_ids,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetCorrectionResult {
    pub bets_affected: usize,
    pub old_winners_reversed: usize,
    pub new_winners_paid: usize,
    pub balance_changes: BTreeMap<i64, i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettledBetRecord {
    pub bet_id: i64,
    pub discord_id: i64,
    pub team: MatchSide,
    pub effective_bet: i64,
    pub payout: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorrectionParticipant {
    pub discord_id: i64,
    pub team: MatchSide,
    /// The exact balance delta persisted at recording/correction time. `None`
    /// identifies a legacy pre-snapshot row and therefore uses the match's
    /// recorded reward when that winner is reversed.
    pub win_bonus_jc: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrectionMatchContext {
    pub match_id: i64,
    pub guild_id: i64,
    pub winning_team: MatchSide,
    /// The match input, including the historical +4 fallback for rows written
    /// before `matches.win_reward_jc` existed.
    pub recorded_win_reward_jc: i64,
    /// Python treats the literal `pool` value and falsey legacy values (NULL
    /// or an empty string) as pool settlement. Other values retain the house
    /// calculation path.
    pub pool_betting_mode: bool,
    pub participants: Vec<CorrectionParticipant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingSnapshot {
    pub discord_id: i64,
    pub rating: Option<f64>,
    pub rating_before: Option<f64>,
    pub rd_before: Option<f64>,
    pub volatility_before: Option<f64>,
    pub os_mu_before: Option<f64>,
    pub os_sigma_before: Option<f64>,
    pub streak_multiplier_per_game: Option<f64>,
    pub streak_threshold: Option<i64>,
    pub base_rating_delta_multiplier: Option<f64>,
    pub low_priority_gain_multiplier: Option<f64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrectionAudit {
    pub correction_id: i64,
    pub old_winning_team: MatchSide,
    pub new_winning_team: MatchSide,
    pub corrected_by: i64,
}

#[derive(Debug, Error)]
pub enum MatchCorrectionError {
    #[error("match correction SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("new_winning_team must be 1 or 2, got {0}")]
    InvalidWinningTeam(i64),
    #[error("match {0} not found")]
    MatchNotFound(i64),
    #[error("match {match_id} already has {side} as winner")]
    AlreadyWinner { match_id: i64, side: &'static str },
    #[error("owner_token is required")]
    MissingOwnerToken,
    #[error("match {0} correction is already in progress")]
    CorrectionInProgress(i64),
    #[error("match {0} has an incompatible correction in progress")]
    IncompatibleClaim(i64),
    #[error("match {0} correction claim is missing or no longer owned")]
    ClaimNotOwned(i64),
    #[error("match {0} winner changed before correction commit")]
    WinnerChanged(i64),
    #[error("participant {discord_id} is missing from match {match_id}")]
    MissingParticipant { match_id: i64, discord_id: i64 },
    #[error("player {discord_id} is missing in guild {guild_id}")]
    MissingPlayer { discord_id: i64, guild_id: i64 },
    #[error("invalid correction claim state: {0}")]
    InvalidClaimState(String),
    #[error("match correction invariant failed: {0}")]
    Invariant(&'static str),
    #[error("rating correction failed: {0}")]
    Rating(String),
}

/// Compile-time persistence contract for correction orchestration. A
/// coordinator generic over this trait cannot be constructed when an
/// exact-once money or ownership primitive is absent.
pub trait MatchCorrectionPersistence {
    fn update_participant_bonus_jc(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        bonus_by_player: &BTreeMap<i64, i64>,
        win_bonus_by_player: &BTreeMap<i64, i64>,
    ) -> Result<(), MatchCorrectionError>;

    fn claim_match_bonuses_paid(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchCorrectionError>;

    fn release_match_bonuses_claim(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchCorrectionError>;

    fn apply_win_bonus_reversal_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        debits_by_player: &BTreeMap<i64, i64>,
    ) -> Result<(), MatchCorrectionError>;

    fn get_win_bonus_credited_ids(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeSet<i64>, MatchCorrectionError>;

    fn claim_match_correction(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
        owner_token: &str,
        now: i64,
        stale_after_seconds: i64,
    ) -> Result<CorrectionClaim, MatchCorrectionError>;

    fn release_match_correction_claim(
        &self,
        match_id: i64,
        owner_token: &str,
    ) -> Result<bool, MatchCorrectionError>;

    fn complete_match_correction_claim(
        &self,
        match_id: i64,
        owner_token: &str,
    ) -> Result<bool, MatchCorrectionError>;
}

pub trait BetCorrectionPersistence {
    fn get_settled_bets_for_match(
        &self,
        match_id: i64,
    ) -> Result<Vec<SettledBetRecord>, MatchCorrectionError>;

    fn settle_bet_correction_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
        options: BetCorrectionOptions<'_>,
    ) -> Result<BetCorrectionResult, MatchCorrectionError>;
}

#[derive(Clone, Debug)]
pub struct MatchCorrectionRepository {
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingOpenSkillReplay {
    pub guild_id: i64,
    pub reason: String,
}

impl MatchCorrectionRepository {
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

    pub fn connection(&self) -> Result<Connection, MatchCorrectionError> {
        open_runtime_connection(&self.path).map_err(Into::into)
    }

    /// Load the immutable match inputs and mutable exact-once bonus snapshots
    /// needed by the correction coordinator after it owns the durable claim.
    pub fn correction_context(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<CorrectionMatchContext, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let match_row = connection
            .query_row(
                "SELECT winning_team,win_reward_jc,betting_mode
                 FROM matches WHERE match_id=?1 AND guild_id=?2",
                params![match_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(MatchCorrectionError::MatchNotFound(match_id))?;
        let mut statement = connection.prepare(
            "SELECT discord_id,team_number,win_bonus_jc
             FROM match_participants
             WHERE match_id=?1 AND guild_id=?2
             ORDER BY discord_id",
        )?;
        let participants = statement
            .query_map(params![match_id, guild_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })?
            .map(|row| {
                let (discord_id, team, win_bonus_jc) = row?;
                Ok(CorrectionParticipant {
                    discord_id,
                    team: MatchSide::try_from(team)?,
                    win_bonus_jc,
                })
            })
            .collect::<Result<Vec<_>, MatchCorrectionError>>()?;
        Ok(CorrectionMatchContext {
            match_id,
            guild_id,
            winning_team: MatchSide::try_from(match_row.0)?,
            recorded_win_reward_jc: recorded_win_reward_jc(match_row.1),
            pool_betting_mode: matches!(match_row.2.as_deref(), None | Some("") | Some("pool")),
            participants,
        })
    }

    /// Recompute the correction's Glicko and OpenSkill rows from persisted
    /// pre-match snapshots. Recording-time streak, base-delta, and low-priority
    /// inputs are authoritative; the compatibility entry point uses default
    /// OpenSkill model parameters.
    pub fn prepare_rating_correction(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
    ) -> Result<Vec<RatingCorrection>, MatchCorrectionError> {
        self.prepare_rating_correction_configured(
            match_id,
            guild_id,
            new_winning_team,
            &CamaOpenSkillSystem::default(),
            &CamaRatingSystem::default(),
        )
    }

    /// Production variant retaining deployment-provided OpenSkill and Glicko
    /// policy (Python reads the same env-derived constants at import time).
    /// Streak/base/gain inputs still come from the recorded match rows.
    pub fn prepare_rating_correction_configured(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
        openskill: &CamaOpenSkillSystem,
        rating_system: &CamaRatingSystem,
    ) -> Result<Vec<RatingCorrection>, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let (radiant_ids, dire_ids) =
            participant_teams_from_connection(&connection, match_id, guild_id)?;
        let participant_ids = radiant_ids
            .iter()
            .chain(&dire_ids)
            .copied()
            .collect::<Vec<_>>();
        if participant_ids.is_empty() {
            return Err(MatchCorrectionError::Invariant(
                "a correction requires persisted match participants",
            ));
        }
        let snapshots = self.rating_snapshots(match_id, Some(guild_id))?;
        let snapshot_by_id = snapshots
            .iter()
            .map(|snapshot| (snapshot.discord_id, snapshot))
            .collect::<BTreeMap<_, _>>();

        // This repository operation is the sole pre-match outcome read for the
        // preparation, retaining Python's one bulk-query invariant.
        let outcomes = RatingHistoryRepository::new(&self.path)
            .get_player_outcomes_before_match_bulk(
                &participant_ids,
                Some(guild_id),
                match_id,
                20,
            )?;
        let mut streaks = HashMap::new();
        let mut gain_multipliers = HashMap::new();
        let mut streak_metadata = BTreeMap::new();
        for &discord_id in &participant_ids {
            let snapshot = snapshot_by_id.get(&discord_id).copied();
            let won = side_for_player(discord_id, &radiant_ids) == new_winning_team;
            let rate = recorded_streak_rate(optional_real(
                snapshot.and_then(|row| row.streak_multiplier_per_game),
            ));
            let threshold = recorded_streak_threshold(optional_integer(
                snapshot.and_then(|row| row.streak_threshold),
            ));
            let adjustment = rating_system.calculate_streak_multiplier(
                outcomes
                    .get(&discord_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                won,
                Some(rate),
                Some(threshold),
            );
            streaks.insert(discord_id, adjustment.multiplier);
            gain_multipliers.insert(
                discord_id,
                snapshot
                    .and_then(|row| row.low_priority_gain_multiplier)
                    .unwrap_or(1.0),
            );
            streak_metadata.insert(
                discord_id,
                (adjustment.streak_length as i64, adjustment.multiplier),
            );
        }
        let base_rating_delta_multiplier = snapshots
            .iter()
            .find_map(|snapshot| snapshot.base_rating_delta_multiplier)
            .unwrap_or(0.75);
        let build_team = |ids: &[i64]| -> Result<Vec<TeamPlayer<i64>>, MatchCorrectionError> {
            ids.iter()
                .map(|&discord_id| {
                    let snapshot = snapshot_by_id.get(&discord_id).copied();
                    let stored = snapshot.and_then(|row| {
                        Some((
                            row.rating_before?,
                            row.rd_before?,
                            row.volatility_before?,
                        ))
                    });
                    let (rating, rd, volatility) = match stored {
                        Some(values) => values,
                        None => connection
                            .query_row(
                                "SELECT glicko_rating,glicko_rd,glicko_volatility
                                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                                params![discord_id, guild_id],
                                |row| {
                                    Ok((
                                        row.get::<_, Option<f64>>(0)?,
                                        row.get::<_, Option<f64>>(1)?,
                                        row.get::<_, Option<f64>>(2)?,
                                    ))
                                },
                            )
                            .optional()?
                            .ok_or(MatchCorrectionError::MissingPlayer {
                                discord_id,
                                guild_id,
                            })
                            .and_then(|(rating, rd, volatility)| {
                                match (rating, rd, volatility) {
                                    (Some(rating), Some(rd), Some(volatility)) => {
                                        Ok((rating, rd, volatility))
                                    }
                                    _ => Err(MatchCorrectionError::Invariant(
                                        "a participant requires a current or pre-match Glicko rating",
                                    )),
                                }
                            })?,
                    };
                    Ok(TeamPlayer::new(
                        rating_system.create_player_from_rating(rating, rd, volatility),
                        discord_id,
                    ))
                })
                .collect()
        };
        let radiant_glicko = build_team(&radiant_ids)?;
        let dire_glicko = build_team(&dire_ids)?;
        let glicko = rating_system
            .update_ratings_after_match_with_options(
                &radiant_glicko,
                &dire_glicko,
                new_winning_team.number() as i32,
                &MatchUpdateOptions {
                    streak_multipliers: streaks,
                    base_rating_delta_multiplier: Some(base_rating_delta_multiplier),
                    gain_multipliers,
                },
            )
            .map_err(|error| MatchCorrectionError::Rating(error.to_string()))?;
        let glicko_by_id = glicko
            .team1
            .into_iter()
            .chain(glicko.team2)
            .map(|update| (update.id, update))
            .collect::<BTreeMap<_, _>>();

        let has_openskill = snapshots
            .iter()
            .any(|snapshot| snapshot.os_mu_before.is_some());
        let mut openskill_by_id = BTreeMap::new();
        if has_openskill {
            let fantasy_points = participant_fantasy_points(&connection, match_id, guild_id)?;
            let to_weighted = |discord_id: i64| -> Result<WeightedPlayer, MatchCorrectionError> {
                let id = u64::try_from(discord_id).map_err(|_| {
                    MatchCorrectionError::Invariant(
                        "Discord IDs must be non-negative for OpenSkill",
                    )
                })?;
                let snapshot = snapshot_by_id.get(&discord_id).copied();
                Ok(WeightedPlayer::new(
                    id,
                    snapshot.and_then(|row| row.os_mu_before),
                    snapshot.and_then(|row| row.os_sigma_before),
                    fantasy_points.get(&discord_id).copied().flatten(),
                ))
            };
            let radiant = radiant_ids
                .iter()
                .copied()
                .map(to_weighted)
                .collect::<Result<Vec<_>, _>>()?;
            let dire = dire_ids
                .iter()
                .copied()
                .map(to_weighted)
                .collect::<Result<Vec<_>, _>>()?;
            let os_streaks = streak_metadata
                .iter()
                .map(|(&discord_id, &(_, multiplier))| {
                    Ok((
                        u64::try_from(discord_id).map_err(|_| {
                            MatchCorrectionError::Invariant(
                                "Discord IDs must be non-negative for OpenSkill",
                            )
                        })?,
                        multiplier,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, MatchCorrectionError>>()?;
            let os_gains = participant_ids
                .iter()
                .map(|&discord_id| {
                    Ok((
                        u64::try_from(discord_id).map_err(|_| {
                            MatchCorrectionError::Invariant(
                                "Discord IDs must be non-negative for OpenSkill",
                            )
                        })?,
                        snapshot_by_id
                            .get(&discord_id)
                            .and_then(|row| row.low_priority_gain_multiplier)
                            .unwrap_or(1.0),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, MatchCorrectionError>>()?;
            openskill_by_id = openskill
                .update_ratings_after_match(
                    &radiant,
                    &dire,
                    if new_winning_team == MatchSide::Radiant {
                        WinningTeam::Team1
                    } else {
                        WinningTeam::Team2
                    },
                    &os_streaks,
                    &os_gains,
                )
                .map_err(|error| MatchCorrectionError::Rating(error.to_string()))?
                .into_iter()
                .map(|(id, update)| {
                    (
                        id as i64,
                        (
                            update.rating.mu,
                            update.rating.sigma,
                            update.contribution_weight,
                        ),
                    )
                })
                .collect();
        }

        participant_ids
            .into_iter()
            .map(|discord_id| {
                let update = &glicko_by_id[&discord_id];
                let won = side_for_player(discord_id, &radiant_ids) == new_winning_team;
                let openskill = openskill_by_id.get(&discord_id).copied();
                let (streak_length, streak_multiplier) = streak_metadata[&discord_id];
                Ok(RatingCorrection {
                    discord_id,
                    rating: update.rating,
                    rd: update.rd,
                    volatility: update.volatility,
                    won,
                    os_mu: openskill.map(|value| value.0),
                    os_sigma: openskill.map(|value| value.1),
                    fantasy_weight: openskill.and_then(|value| value.2),
                    streak_length: Some(streak_length),
                    streak_multiplier: Some(streak_multiplier),
                })
            })
            .collect()
    }

    pub fn claim_match_correction(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
        owner_token: &str,
        now: i64,
        stale_after_seconds: i64,
    ) -> Result<CorrectionClaim, MatchCorrectionError> {
        if owner_token.is_empty() {
            return Err(MatchCorrectionError::MissingOwnerToken);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_winner = transaction
            .query_row(
                "SELECT winning_team FROM matches WHERE match_id=?1 AND guild_id=?2",
                params![match_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(MatchCorrectionError::MatchNotFound(match_id))?;
        let current_winner = MatchSide::try_from(current_winner)?;
        let existing = transaction
            .query_row(
                "SELECT guild_id,old_winning_team,new_winning_team,state,
                        owner_token,claimed_at,correction_id
                 FROM match_correction_claims WHERE match_id=?1",
                [match_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .optional()?;

        let claim = if let Some((
            claim_guild,
            old_winner,
            requested_winner,
            state,
            active_owner,
            claimed_at,
            correction_id,
        )) = existing
        {
            let old_winner = MatchSide::try_from(old_winner)?;
            let requested_winner = MatchSide::try_from(requested_winner)?;
            if claim_guild != guild_id
                || requested_winner != new_winning_team
                || !matches_current_or_requested(current_winner, old_winner, requested_winner)
            {
                return Err(MatchCorrectionError::IncompatibleClaim(match_id));
            }
            let stale_after_seconds = stale_after_seconds.max(1);
            if active_owner
                .as_deref()
                .is_some_and(|token| token != owner_token)
                && claimed_at > now - stale_after_seconds
            {
                return Err(MatchCorrectionError::CorrectionInProgress(match_id));
            }
            transaction.execute(
                "UPDATE match_correction_claims
                 SET owner_token=?1,claimed_at=?2 WHERE match_id=?3",
                params![owner_token, now, match_id],
            )?;
            CorrectionClaim {
                match_id,
                guild_id,
                old_winning_team: old_winner,
                new_winning_team: requested_winner,
                state: ClaimState::parse(&state)?,
                correction_id,
            }
        } else if current_winner == new_winning_team {
            CorrectionClaim {
                match_id,
                guild_id,
                old_winning_team: current_winner,
                new_winning_team,
                state: ClaimState::Already,
                correction_id: None,
            }
        } else {
            transaction.execute(
                "INSERT INTO match_correction_claims (
                     match_id,guild_id,old_winning_team,new_winning_team,
                     state,owner_token,claimed_at
                 ) VALUES (?1,?2,?3,?4,'pending',?5,?6)",
                params![
                    match_id,
                    guild_id,
                    current_winner.number(),
                    new_winning_team.number(),
                    owner_token,
                    now
                ],
            )?;
            CorrectionClaim {
                match_id,
                guild_id,
                old_winning_team: current_winner,
                new_winning_team,
                state: ClaimState::Pending,
                correction_id: None,
            }
        };
        transaction.commit()?;
        Ok(claim)
    }

    pub fn release_match_correction_claim(
        &self,
        match_id: i64,
        owner_token: &str,
    ) -> Result<bool, MatchCorrectionError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE match_correction_claims SET owner_token=NULL,claimed_at=0
             WHERE match_id=?1 AND owner_token=?2",
            params![match_id, owner_token],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    /// Release every process-local correction lease during single-process
    /// startup recovery. The durable `pending`/`core_applied` state and replay
    /// jobs remain untouched; only an owner token from the dead process is
    /// cleared. Production calls this on the first READY recovery pass after
    /// acquiring the process lock, so no live correction can be displaced.
    pub fn release_match_correction_leases_for_startup(
        &self,
    ) -> Result<usize, MatchCorrectionError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE match_correction_claims SET owner_token=NULL,claimed_at=0
             WHERE owner_token IS NOT NULL",
            [],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    /// Release dead-process correction leases only for guilds present in the
    /// current gateway READY snapshot. A dev bot may intentionally open a
    /// production database copy without belonging to those production guilds.
    pub fn release_match_correction_leases_for_startup_guilds(
        &self,
        guild_ids: &BTreeSet<i64>,
    ) -> Result<usize, MatchCorrectionError> {
        if guild_ids.is_empty() {
            return Ok(0);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0usize;
        for guild_id in guild_ids {
            changed = changed.saturating_add(transaction.execute(
                "UPDATE match_correction_claims SET owner_token=NULL,claimed_at=0
                 WHERE owner_token IS NOT NULL AND guild_id=?1",
                [guild_id],
            )?);
        }
        transaction.commit()?;
        Ok(changed)
    }

    /// Enumerate every correction that has not reached claim cleanup.
    ///
    /// A process can exit while the claim is still `pending` after one or more
    /// exact-once winner rewards committed, or after the core flip/replay while
    /// the claim is `core_applied`. Both states therefore need READY recovery;
    /// treating pre-core claims as command-only would strand partially moved
    /// money indefinitely after a hard exit.
    pub fn list_recoverable_match_corrections(
        &self,
    ) -> Result<Vec<CorrectionClaim>, MatchCorrectionError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT match_id,guild_id,old_winning_team,new_winning_team,
                    state,correction_id
             FROM match_correction_claims
             WHERE state IN ('pending','core_applied')
               AND owner_token IS NULL
             ORDER BY match_id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(match_id, guild_id, old_winner, new_winner, state, correction_id)| {
                    Ok(CorrectionClaim {
                        match_id,
                        guild_id,
                        old_winning_team: MatchSide::try_from(old_winner)?,
                        new_winning_team: MatchSide::try_from(new_winner)?,
                        state: ClaimState::parse(&state)?,
                        correction_id,
                    })
                },
            )
            .collect()
    }

    pub fn complete_match_correction_claim(
        &self,
        match_id: i64,
        owner_token: &str,
    ) -> Result<bool, MatchCorrectionError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "DELETE FROM match_correction_claims
             WHERE match_id=?1 AND owner_token=?2 AND state='core_applied'",
            params![match_id, owner_token],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    /// Persist one service-resolved match-win balance movement. Garnishment,
    /// bankruptcy, vanity tax, buffs, and hostile skims remain service policy;
    /// the caller supplies their already-resolved balance delta rather than a
    /// gross configured reward. The ledger trigger makes this credit itself an
    /// exact-once marker even though the participant snapshot is a later seam.
    pub fn credit_resolved_win_bonus(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        discord_id: i64,
        balance_delta: i64,
    ) -> Result<bool, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = transaction
            .query_row(
                "SELECT win_bonus_jc FROM match_participants
                 WHERE match_id=?1 AND discord_id=?2 AND guild_id=?3",
                params![match_id, discord_id, guild_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .ok_or(MatchCorrectionError::MissingParticipant {
                match_id,
                discord_id,
            })?;
        if stored.is_some_and(|amount| amount > 0)
            || (stored.is_none()
                && win_bonus_ledger_marker_exists(&transaction, match_id, guild_id, discord_id)?)
        {
            transaction.commit()?;
            return Ok(false);
        }

        install_ledger_context(
            &transaction,
            "match_win_bonus",
            "match_win_bonus",
            match_id,
            "match win bonus",
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![balance_delta, discord_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        if changed != 1 {
            return Err(MatchCorrectionError::MissingPlayer {
                discord_id,
                guild_id,
            });
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn snapshot_win_bonus(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        discord_id: i64,
        amount: i64,
    ) -> Result<(), MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE match_participants SET win_bonus_jc=?1
             WHERE match_id=?2 AND discord_id=?3 AND guild_id=?4",
            params![amount, match_id, discord_id, guild_id],
        )?;
        if changed != 1 {
            return Err(MatchCorrectionError::MissingParticipant {
                match_id,
                discord_id,
            });
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn update_participant_bonus_jc(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        bonus_by_player: &BTreeMap<i64, i64>,
        win_bonus_by_player: &BTreeMap<i64, i64>,
    ) -> Result<(), MatchCorrectionError> {
        if bonus_by_player.is_empty() && win_bonus_by_player.is_empty() {
            return Ok(());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (&discord_id, &amount) in bonus_by_player {
            transaction.execute(
                "UPDATE match_participants SET bonus_jc=?1
                 WHERE match_id=?2 AND discord_id=?3 AND guild_id=?4",
                params![amount, match_id, discord_id, guild_id],
            )?;
        }
        for (&discord_id, &amount) in win_bonus_by_player {
            transaction.execute(
                "UPDATE match_participants SET win_bonus_jc=?1
                 WHERE match_id=?2 AND discord_id=?3 AND guild_id=?4",
                params![amount, match_id, discord_id, guild_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn claim_match_bonuses_paid(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchCorrectionError> {
        self.set_match_bonus_claim(match_id, guild_id, 0, 1)
    }

    pub fn release_match_bonuses_claim(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchCorrectionError> {
        self.set_match_bonus_claim(match_id, guild_id, 1, 0)
    }

    fn set_match_bonus_claim(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        expected: i64,
        replacement: i64,
    ) -> Result<bool, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE matches SET bonuses_paid=?1
             WHERE match_id=?2 AND guild_id=?3
               AND COALESCE(bonuses_paid,0)=?4",
            params![replacement, match_id, guild_id, expected],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn win_bonus_credited_ids(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeSet<i64>, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT account_id FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_type='player'
               AND related_type='match_win_bonus' AND related_id=?2
               AND account_id IS NOT NULL",
        )?;
        statement
            .query_map(params![guild_id, match_id.to_string()], |row| row.get(0))?
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(Into::into)
    }

    /// Debit win bonuses back off the previous winners during a correction.
    ///
    /// Each debit is recorded as a `match_bonus_rollback` compensation of the
    /// original `match_win_bonus` award. That row shape is what
    /// `income_award_marker_exists` looks for when deciding whether the
    /// exact-once award marker still stands: without it a later correction back
    /// to the original winner sees a live marker, skips the credit, and yet
    /// snapshots the historical amount as paid -- so the players lose the bonus
    /// permanently and a further correction debits it a second time.
    pub fn reverse_win_bonuses_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        debits_by_player: &BTreeMap<i64, i64>,
    ) -> Result<(), MatchCorrectionError> {
        if debits_by_player.is_empty() {
            return Ok(());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (&discord_id, &amount) in debits_by_player {
            transaction.execute("DELETE FROM economy_ledger_context", [])?;
            transaction.execute(
                "INSERT INTO economy_ledger_context (
                     id,source,related_type,related_id,reason,metadata
                 ) VALUES (1,'match_bonus_rollback','match_win_bonus',?1,
                           'match bonus rollback',
                           '{\"compensates_source\":\"match_win_bonus\"}')",
                params![match_id.to_string()],
            )?;
            let changed = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![amount, discord_id, guild_id],
            )?;
            if changed != 1 {
                return Err(MatchCorrectionError::MissingPlayer {
                    discord_id,
                    guild_id,
                });
            }
            transaction.execute(
                "UPDATE players SET lowest_balance_ever=jopacoin_balance
                 WHERE discord_id=?1 AND guild_id=?2
                   AND (lowest_balance_ever IS NULL
                        OR jopacoin_balance<lowest_balance_ever)",
                params![discord_id, guild_id],
            )?;
            let participant_changed = transaction.execute(
                "UPDATE match_participants SET win_bonus_jc=0
                 WHERE match_id=?1 AND discord_id=?2 AND guild_id=?3",
                params![match_id, discord_id, guild_id],
            )?;
            if participant_changed != 1 {
                return Err(MatchCorrectionError::MissingParticipant {
                    match_id,
                    discord_id,
                });
            }
        }
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn apply_core_correction(
        &self,
        request: &CoreCorrectionRequest<'_>,
    ) -> Result<Option<i64>, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_owned_pending_claim(&transaction, request, guild_id)?;
        if request
            .rating_updates
            .iter()
            .any(|update| update.os_mu.is_some() != update.os_sigma.is_some())
        {
            return Err(MatchCorrectionError::Invariant(
                "rating updates require history and paired OpenSkill values",
            ));
        }
        let (radiant_ids, dire_ids) = participant_teams(&transaction, request.match_id, guild_id)?;
        let participant_ids = radiant_ids
            .iter()
            .chain(&dire_ids)
            .copied()
            .collect::<BTreeSet<_>>();
        let update_ids = request
            .rating_updates
            .iter()
            .map(|update| update.discord_id)
            .collect::<BTreeSet<_>>();
        if !request.rating_updates.is_empty()
            && (participant_ids != update_ids || update_ids.len() != request.rating_updates.len())
        {
            return Err(MatchCorrectionError::Invariant(
                "rating updates must cover every participant exactly once",
            ));
        }
        let changed = transaction.execute(
            "UPDATE matches SET winning_team=?1
             WHERE match_id=?2 AND guild_id=?3 AND winning_team=?4",
            params![
                request.new_winning_team.number(),
                request.match_id,
                guild_id,
                request.old_winning_team.number()
            ],
        )?;
        if changed != 1 {
            return Err(MatchCorrectionError::WinnerChanged(request.match_id));
        }
        let (old_winners, old_losers) = if request.old_winning_team == MatchSide::Radiant {
            (&radiant_ids, &dire_ids)
        } else {
            (&dire_ids, &radiant_ids)
        };
        for &discord_id in old_winners {
            transaction.execute(
                "UPDATE players SET wins=wins-1,losses=losses+1,
                                    updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
            )?;
        }
        for &discord_id in old_losers {
            transaction.execute(
                "UPDATE players SET losses=losses-1,wins=wins+1,
                                    updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
            )?;
        }

        for update in request.rating_updates {
            transaction.execute(
                "UPDATE players
                 SET glicko_rating=?1,glicko_rd=?2,glicko_volatility=?3,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?4 AND guild_id=?5
                   AND NOT EXISTS (
                       SELECT 1 FROM rating_history rh
                       JOIN matches later
                         ON later.match_id=rh.match_id AND later.guild_id=rh.guild_id
                       JOIN matches target
                         ON target.match_id=?6 AND target.guild_id=players.guild_id
                       WHERE rh.discord_id=players.discord_id
                         AND rh.guild_id=players.guild_id
                         AND (later.match_date>target.match_date OR
                              (later.match_date=target.match_date
                               AND later.match_id>target.match_id))
                   )",
                params![
                    update.rating,
                    cap_glicko_rd(update.rd),
                    update.volatility,
                    update.discord_id,
                    guild_id,
                    request.match_id
                ],
            )?;
            if let (Some(mu), Some(sigma)) = (update.os_mu, update.os_sigma) {
                transaction.execute(
                    "UPDATE players
                     SET os_mu=?1,os_sigma=?2,os_rating_version=?3,
                         os_algorithm_fingerprint=?4,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?5 AND guild_id=?6",
                    params![
                        mu,
                        sigma,
                        request.openskill_algorithm_version,
                        request.openskill_algorithm_fingerprint,
                        update.discord_id,
                        guild_id
                    ],
                )?;
            }
            transaction.execute(
                "UPDATE rating_history
                 SET rating=?1,rd_after=?2,volatility_after=?3,won=?4,
                     os_mu_after=COALESCE(?5,os_mu_after),
                     os_sigma_after=COALESCE(?6,os_sigma_after),
                     fantasy_weight=?7,os_algorithm_version=?8,
                     os_algorithm_fingerprint=?9,
                     streak_length=COALESCE(?10,streak_length),
                     streak_multiplier=COALESCE(?11,streak_multiplier)
                 WHERE match_id=?12 AND discord_id=?13 AND guild_id=?14",
                params![
                    update.rating,
                    update.rd,
                    update.volatility,
                    update.won,
                    update.os_mu,
                    update.os_sigma,
                    update.fantasy_weight,
                    request.openskill_algorithm_version,
                    request.openskill_algorithm_fingerprint,
                    update.streak_length,
                    update.streak_multiplier,
                    request.match_id,
                    update.discord_id,
                    guild_id
                ],
            )?;
        }

        transaction.execute(
            "UPDATE match_participants
             SET won=CASE WHEN team_number=?1 THEN 1 ELSE 0 END
             WHERE match_id=?2 AND guild_id=?3",
            params![
                request.new_winning_team.number(),
                request.match_id,
                guild_id
            ],
        )?;
        apply_pairing_win_deltas(
            &transaction,
            guild_id,
            &radiant_ids,
            &dire_ids,
            request.old_winning_team,
            request.new_winning_team,
        )?;

        let correction_id = if let Some(corrected_by) = request.corrected_by {
            transaction.execute(
                "INSERT INTO match_corrections (
                     match_id,old_winning_team,new_winning_team,corrected_by
                 ) VALUES (?1,?2,?3,?4)",
                params![
                    request.match_id,
                    request.old_winning_team.number(),
                    request.new_winning_team.number(),
                    corrected_by
                ],
            )?;
            Some(transaction.last_insert_rowid())
        } else {
            None
        };
        transaction.execute(
            "INSERT INTO openskill_replay_jobs (
                 guild_id,reason,requested_at,last_error
             ) VALUES (?1,?2,CURRENT_TIMESTAMP,NULL)
             ON CONFLICT(guild_id) DO UPDATE SET
                 reason=excluded.reason,requested_at=excluded.requested_at,last_error=NULL",
            params![guild_id, format!("match_correction:{}", request.match_id)],
        )?;
        if request
            .rating_updates
            .iter()
            .any(|update| update.os_mu.is_some())
        {
            transaction.execute(
                "INSERT INTO openskill_rating_revisions (guild_id,revision,updated_at)
                 VALUES (?1,1,CURRENT_TIMESTAMP)
                 ON CONFLICT(guild_id) DO UPDATE SET
                    revision=openskill_rating_revisions.revision+1,
                    updated_at=CURRENT_TIMESTAMP",
                [guild_id],
            )?;
        }
        let claim_changed = transaction.execute(
            "UPDATE match_correction_claims
             SET state='core_applied',correction_id=?1
             WHERE match_id=?2 AND owner_token=?3 AND state='pending'",
            params![correction_id, request.match_id, request.owner_token],
        )?;
        if claim_changed != 1 {
            return Err(MatchCorrectionError::ClaimNotOwned(request.match_id));
        }
        transaction.commit()?;
        Ok(correction_id)
    }

    pub fn get_settled_bets_for_match(
        &self,
        match_id: i64,
    ) -> Result<Vec<SettledBetRecord>, MatchCorrectionError> {
        let connection = self.connection()?;
        load_bets(&connection, match_id).map(|bets| {
            bets.into_iter()
                .map(|bet| SettledBetRecord {
                    bet_id: bet.bet_id,
                    discord_id: bet.discord_id,
                    team: bet.team,
                    effective_bet: bet.effective_bet,
                    payout: bet.payout,
                })
                .collect()
        })
    }

    pub fn settle_bet_correction_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
        options: BetCorrectionOptions<'_>,
    ) -> Result<BetCorrectionResult, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let bets = load_bets(&transaction, match_id)?;
        let payout_pattern_complete = bets
            .iter()
            .all(|bet| bet.payout.is_some() == (bet.team == new_winning_team));
        if payout_pattern_complete {
            let old_winners_reversed = bets
                .iter()
                .filter(|bet| bet.team != new_winning_team)
                .count();
            let new_winners_paid = bets.len() - old_winners_reversed;
            transaction.commit()?;
            return Ok(BetCorrectionResult {
                bets_affected: bets.len(),
                old_winners_reversed,
                new_winners_paid,
                balance_changes: BTreeMap::new(),
            });
        }
        let old_winners = bets
            .iter()
            .filter(|bet| bet.team != new_winning_team)
            .collect::<Vec<_>>();
        let new_winners = bets
            .iter()
            .filter(|bet| bet.team == new_winning_team)
            .collect::<Vec<_>>();
        let (new_deltas, payout_updates) = compute_new_payouts(
            &bets,
            &old_winners,
            &new_winners,
            options.pool_mode,
            options.house_payout_multiplier,
        );
        let mut combined_deltas = BTreeMap::new();
        for bet in &old_winners {
            if let Some(payout) = bet.payout.filter(|payout| *payout > 0) {
                *combined_deltas.entry(bet.discord_id).or_insert(0) -= payout;
            }
        }
        for (&discord_id, &payout) in &new_deltas {
            *combined_deltas.entry(discord_id).or_insert(0) += payout;
        }

        let old_taxes = load_settlement_taxes(&transaction, match_id, guild_id)?;
        for (&discord_id, taxes) in &old_taxes {
            *combined_deltas.entry(discord_id).or_insert(0) += taxes.total();
        }
        let total_stakes = total_stakes_by_player(&bets);
        let new_taxes = new_deltas
            .iter()
            .filter_map(|(&discord_id, &payout)| {
                let profit = payout - total_stakes.get(&discord_id).copied().unwrap_or(0);
                if profit <= 0 {
                    return None;
                }
                let vanity_tax = if options.vanity_taxable_ids.contains(&discord_id) {
                    (profit as f64 * options.vanity_tax_rate) as i64
                } else {
                    0
                };
                // Both withholdings are charged against the same pre-tax
                // profit, so misconfigured rates could otherwise take more
                // than the player won.  Vanity totals are already visible to
                // players, so the clamp falls on the low-priority share.
                let low_priority_tax = if options.low_priority_taxable_ids.contains(&discord_id) {
                    ((profit as f64 * options.low_priority_tax_rate) as i64)
                        .clamp(0, (profit - vanity_tax).max(0))
                } else {
                    0
                };
                let taxes = SettlementTaxes {
                    vanity_tax,
                    low_priority_tax,
                };
                (taxes.total() > 0).then_some((discord_id, taxes))
            })
            .collect::<BTreeMap<_, _>>();
        for (&discord_id, taxes) in &new_taxes {
            *combined_deltas.entry(discord_id).or_insert(0) -= taxes.total();
        }

        if !combined_deltas.is_empty() {
            install_ledger_context(
                &transaction,
                "bet_correction",
                "match",
                match_id,
                "corrected bet settlement",
            )?;
            for (&discord_id, &delta) in &combined_deltas {
                let changed = transaction.execute(
                    "UPDATE players
                     SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                         updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![delta, discord_id, guild_id],
                )?;
                if changed != 1 {
                    return Err(MatchCorrectionError::MissingPlayer {
                        discord_id,
                        guild_id,
                    });
                }
                if delta < 0 {
                    transaction.execute(
                        "UPDATE players SET lowest_balance_ever=jopacoin_balance
                         WHERE discord_id=?1 AND guild_id=?2
                           AND (lowest_balance_ever IS NULL
                                OR jopacoin_balance<lowest_balance_ever)",
                        params![discord_id, guild_id],
                    )?;
                }
            }
            clear_ledger_context(&transaction)?;
        }

        transaction.execute(
            "DELETE FROM bet_settlement_taxes WHERE match_id=?1 AND guild_id=?2",
            params![match_id, guild_id],
        )?;
        for (&discord_id, taxes) in &new_taxes {
            transaction.execute(
                "INSERT INTO bet_settlement_taxes (
                     match_id,guild_id,discord_id,vanity_tax,low_priority_tax
                 ) VALUES (?1,?2,?3,?4,?5)",
                params![
                    match_id,
                    guild_id,
                    discord_id,
                    taxes.vanity_tax,
                    taxes.low_priority_tax
                ],
            )?;
        }
        transaction.execute("UPDATE bets SET payout=NULL WHERE match_id=?1", [match_id])?;
        for (&bet_id, &payout) in &payout_updates {
            transaction.execute(
                "UPDATE bets SET payout=?1 WHERE bet_id=?2 AND match_id=?3",
                params![payout, bet_id, match_id],
            )?;
        }
        transaction.commit()?;
        Ok(BetCorrectionResult {
            bets_affected: bets.len(),
            old_winners_reversed: old_winners.len(),
            new_winners_paid: new_winners.len(),
            balance_changes: combined_deltas,
        })
    }

    pub fn bet_correction_complete(
        &self,
        match_id: i64,
        new_winning_team: MatchSide,
    ) -> Result<bool, MatchCorrectionError> {
        let connection = self.connection()?;
        let bets = load_bets(&connection, match_id)?;
        Ok(bets
            .iter()
            .all(|bet| bet.payout.is_some() == (bet.team == new_winning_team)))
    }

    pub fn pending_openskill_replay(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<String>, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT reason FROM openskill_replay_jobs WHERE guild_id=?1",
                [guild_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Enumerate every durable replay request for startup recovery, in the
    /// same deterministic order as Python's service-container composition.
    pub fn list_pending_openskill_replays(
        &self,
    ) -> Result<Vec<PendingOpenSkillReplay>, MatchCorrectionError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT guild_id,reason FROM openskill_replay_jobs
             ORDER BY requested_at,guild_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(PendingOpenSkillReplay {
                    guild_id: row.get(0)?,
                    reason: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Durably request an administrative OpenSkill replay.
    ///
    /// The request is committed under a writer lock before the replay worker
    /// starts, so a crash can never lose the fact that ratings need rebuilding.
    pub fn mark_openskill_replay_pending(
        &self,
        guild_id: Option<i64>,
        reason: &str,
    ) -> Result<(), MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO openskill_replay_jobs (
                 guild_id,reason,requested_at,last_error
             ) VALUES (?1,?2,CURRENT_TIMESTAMP,NULL)
             ON CONFLICT(guild_id) DO UPDATE SET
                 reason=excluded.reason,
                 requested_at=excluded.requested_at,
                 last_error=NULL",
            params![guild_id, reason],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Rebuild a guild's OpenSkill state from every persisted match and apply
    /// the result atomically. Loading and calculation remain under the same
    /// writer lock as persistence so a newly-recorded match cannot race the
    /// replay snapshot. Recording-time streak and low-priority values remain
    /// authoritative throughout the rebuild.
    pub fn replay_openskill_atomic(
        &self,
        guild_id: Option<i64>,
    ) -> Result<OpenSkillReplaySummary, MatchCorrectionError> {
        let system = CamaOpenSkillSystem::default();
        self.replay_openskill_atomic_configured(
            guild_id,
            &system,
            &CamaRatingSystem::default(),
            NEW_PLAYER_MMR_DISCOUNT,
        )
        .map(|(summary, _)| summary)
    }

    /// Configured production replay used by the lift-and-shift runtime.
    ///
    /// Returns the processed summary together with the complete number of
    /// eligible historical matches, matching Python's backfill result shape.
    pub fn replay_openskill_atomic_configured(
        &self,
        guild_id: Option<i64>,
        system: &CamaOpenSkillSystem,
        rating_system: &CamaRatingSystem,
        new_player_mmr_discount: i32,
    ) -> Result<(OpenSkillReplaySummary, usize), MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = transaction
            .query_row(
                "SELECT 1 FROM openskill_replay_jobs WHERE guild_id=?1",
                [guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !pending {
            return Err(MatchCorrectionError::Invariant(
                "OpenSkill replay has no durable pending job",
            ));
        }

        let players = load_replay_players(&transaction, guild_id)?;
        let matches = load_replay_matches(&transaction, guild_id)?;
        let total_matches = matches.len();
        let participants = load_replay_participants(&transaction, guild_id)?;
        let events = load_replay_events(&transaction, guild_id)?;
        let mut current = players
            .iter()
            .map(|player| {
                let source_mmr = player.initial_mmr.unwrap_or(4_000).clamp(0, 12_000);
                let seed_mmr = (source_mmr - i64::from(new_player_mmr_discount)).max(0);
                (
                    player.discord_id,
                    OpenSkillRating::new(
                        system.mmr_to_os_mu(seed_mmr as i32),
                        CamaOpenSkillSystem::DEFAULT_SIGMA,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut recent_outcomes = BTreeMap::<i64, Vec<bool>>::new();
        let mut last_match_at = BTreeMap::<i64, f64>::new();
        let mut history_rows = Vec::new();
        let mut prediction_rows = Vec::new();
        let mut players_touched = BTreeSet::new();
        let mut errors = Vec::new();
        let mut event_offset = 0;
        let mut matches_processed = 0;
        let mut matches_with_fantasy = 0;
        let mut matches_equal_weight = 0;

        for replay_match in &matches {
            apply_replay_events_through(
                &events,
                &mut event_offset,
                replay_match.match_at,
                &mut current,
                &mut players_touched,
                &mut errors,
            );
            let match_participants = participants
                .get(&replay_match.match_id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let (radiant_ids, dire_ids) = match replay_team_ids(replay_match, match_participants) {
                Ok(teams) => teams,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
            let all_ids = radiant_ids
                .iter()
                .chain(&dire_ids)
                .copied()
                .collect::<Vec<_>>();
            let participants_by_id = match_participants
                .iter()
                .map(|participant| (participant.discord_id, participant))
                .collect::<BTreeMap<_, _>>();
            let complete_fantasy = participants_by_id.len() >= 10
                && all_ids.iter().all(|discord_id| {
                    participants_by_id
                        .get(discord_id)
                        .and_then(|participant| participant.fantasy_points)
                        .is_some()
                });

            let mut before = BTreeMap::new();
            for &discord_id in &all_ids {
                let mut rating = current.get(&discord_id).copied().unwrap_or_else(|| {
                    OpenSkillRating::new(
                        CamaOpenSkillSystem::DEFAULT_MU,
                        CamaOpenSkillSystem::DEFAULT_SIGMA,
                    )
                });
                if let (Some(match_at), Some(previous_at)) =
                    (replay_match.match_at, last_match_at.get(&discord_id))
                {
                    let days_since = ((match_at - previous_at).floor() as i64).max(0);
                    rating.sigma = system.apply_sigma_decay(rating.sigma, days_since);
                }
                before.insert(discord_id, rating);
            }

            let rate = recorded_streak_rate(optional_real(replay_match.streak_rate));
            let threshold =
                recorded_streak_threshold(optional_integer(replay_match.streak_threshold));
            let streak_multipliers = match replay_streak_multipliers(
                rating_system,
                &all_ids,
                &radiant_ids,
                replay_match.winning_team,
                &recent_outcomes,
                rate,
                threshold,
            ) {
                Ok(multipliers) => multipliers,
                Err(discord_id) => {
                    errors.push(format!(
                        "Match {}: invalid Discord ID {discord_id}",
                        replay_match.match_id
                    ));
                    continue;
                }
            };
            let gain_multipliers = all_ids
                .iter()
                .map(|discord_id| {
                    let openskill_id = u64::try_from(*discord_id)
                        .expect("replay streak validation accepted Discord ID");
                    let multiplier = participants_by_id
                        .get(discord_id)
                        .map(|participant| participant.low_priority_gain_multiplier)
                        .filter(|multiplier| *multiplier != 0.0)
                        .unwrap_or(1.0);
                    (openskill_id, multiplier)
                })
                .collect::<BTreeMap<_, _>>();

            let build_team = |ids: &[i64]| {
                ids.iter()
                    .map(|discord_id| {
                        let rating = before[discord_id];
                        WeightedPlayer::new(
                            *discord_id as u64,
                            Some(rating.mu),
                            Some(rating.sigma),
                            complete_fantasy
                                .then(|| participants_by_id[discord_id].fantasy_points)
                                .flatten(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let radiant = build_team(&radiant_ids);
            let dire = build_team(&dire_ids);
            let radiant_prediction = radiant_ids
                .iter()
                .map(|discord_id| before[discord_id])
                .collect::<Vec<_>>();
            let dire_prediction = dire_ids
                .iter()
                .map(|discord_id| before[discord_id])
                .collect::<Vec<_>>();
            let raw_probability =
                system.os_predict_win_probability(&radiant_prediction, &dire_prediction);
            let calibrated_probability = system
                .calibrate_win_probability(raw_probability, None)
                .map_err(|error| MatchCorrectionError::Rating(error.to_string()))?;
            prediction_rows.push(ReplayPrediction {
                match_id: replay_match.match_id,
                raw_probability,
                calibrated_probability,
            });
            let updates = match system.update_ratings_after_match(
                &radiant,
                &dire,
                if replay_match.winning_team == MatchSide::Radiant {
                    WinningTeam::Team1
                } else {
                    WinningTeam::Team2
                },
                &streak_multipliers,
                &gain_multipliers,
            ) {
                Ok(updates) => updates,
                Err(error) => {
                    errors.push(format!("Match {}: {error}", replay_match.match_id));
                    prediction_rows.pop();
                    continue;
                }
            };
            if complete_fantasy {
                matches_with_fantasy += 1;
            } else {
                matches_equal_weight += 1;
            }
            for (team, ids) in [
                (MatchSide::Radiant, radiant_ids.as_slice()),
                (MatchSide::Dire, dire_ids.as_slice()),
            ] {
                for &discord_id in ids {
                    let openskill_id = discord_id as u64;
                    let old_rating = before[&discord_id];
                    let update = updates[&openskill_id];
                    current.insert(discord_id, update.rating);
                    players_touched.insert(discord_id);
                    if let Some(match_at) = replay_match.match_at {
                        last_match_at.insert(discord_id, match_at);
                    }
                    let won = team == replay_match.winning_team;
                    history_rows.push(ReplayHistoryRow {
                        match_id: replay_match.match_id,
                        match_date: replay_match.match_date.clone(),
                        discord_id,
                        team_number: team.number(),
                        won,
                        before: old_rating,
                        after: update.rating,
                        fantasy_weight: update.contribution_weight,
                    });
                    record_replay_outcome(&mut recent_outcomes, discord_id, won);
                }
            }
            matches_processed += 1;
        }
        apply_replay_events_through(
            &events,
            &mut event_offset,
            None,
            &mut current,
            &mut players_touched,
            &mut errors,
        );

        let mut summary = OpenSkillReplaySummary {
            matches_processed,
            matches_with_fantasy,
            matches_equal_weight,
            players_updated: players_touched.len(),
            errors,
        };
        if !summary.errors.is_empty() {
            transaction.execute(
                "UPDATE openskill_replay_jobs SET last_error=?1 WHERE guild_id=?2",
                params![
                    summary
                        .errors
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; "),
                    guild_id
                ],
            )?;
            transaction.commit()?;
            summary.errors.truncate(10);
            return Ok((summary, total_matches));
        }

        let fingerprint = system.algorithm_fingerprint();
        persist_openskill_replay(
            &transaction,
            guild_id,
            &history_rows,
            &prediction_rows,
            &current,
            &fingerprint,
        )?;
        transaction.execute(
            "INSERT INTO openskill_rating_revisions(guild_id,revision,updated_at)
             VALUES (?1,1,CURRENT_TIMESTAMP)
             ON CONFLICT(guild_id) DO UPDATE SET
                 revision=openskill_rating_revisions.revision+1,
                 updated_at=CURRENT_TIMESTAMP",
            [guild_id],
        )?;
        let cleared = transaction.execute(
            "DELETE FROM openskill_replay_jobs WHERE guild_id=?1",
            [guild_id],
        )?;
        if cleared != 1 {
            return Err(MatchCorrectionError::Invariant(
                "OpenSkill replay job changed before commit",
            ));
        }
        transaction.commit()?;
        if total_matches == 0 {
            summary.players_updated = current.len();
        }
        Ok((summary, total_matches))
    }

    /// Compatibility boundary for an external replay worker that has already
    /// durably persisted its result. New Rust callers should prefer
    /// [`Self::replay_openskill_atomic`].
    pub fn complete_openskill_replay(
        &self,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "DELETE FROM openskill_replay_jobs WHERE guild_id=?1",
            [guild_id],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    /// Persist a replay result and clear its durable job in the same writer
    /// transaction. Chronological replay computation is supplied by the
    /// coordinator; this boundary prevents a cleared job from disagreeing
    /// with history/live ratings after a crash.
    pub fn apply_openskill_replay_atomic(
        &self,
        guild_id: Option<i64>,
        updates: &[OpenSkillReplayUpdate],
        algorithm_version: i64,
        algorithm_fingerprint: &str,
    ) -> Result<(), MatchCorrectionError> {
        if updates.is_empty() {
            return Err(MatchCorrectionError::Invariant(
                "an OpenSkill replay requires persisted updates",
            ));
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = transaction
            .query_row(
                "SELECT 1 FROM openskill_replay_jobs WHERE guild_id=?1",
                [guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !pending {
            return Err(MatchCorrectionError::Invariant(
                "OpenSkill replay has no durable pending job",
            ));
        }
        for update in updates {
            transaction.execute(
                "UPDATE rating_history
                 SET os_mu_after=?1,os_sigma_after=?2,streak_length=?3,
                     streak_multiplier=?4,os_algorithm_version=?5,
                     os_algorithm_fingerprint=?6
                 WHERE match_id=?7 AND discord_id=?8 AND guild_id=?9",
                params![
                    update.mu,
                    update.sigma,
                    update.streak_length,
                    update.streak_multiplier,
                    algorithm_version,
                    algorithm_fingerprint,
                    update.match_id,
                    update.discord_id,
                    guild_id
                ],
            )?;
            if update.is_live_rating {
                transaction.execute(
                    "UPDATE players
                     SET os_mu=?1,os_sigma=?2,os_rating_version=?3,
                         os_algorithm_fingerprint=?4,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?5 AND guild_id=?6",
                    params![
                        update.mu,
                        update.sigma,
                        algorithm_version,
                        algorithm_fingerprint,
                        update.discord_id,
                        guild_id
                    ],
                )?;
            }
        }
        transaction.execute(
            "INSERT INTO openskill_rating_revisions(guild_id,revision,updated_at)
             VALUES (?1,1,CURRENT_TIMESTAMP)
             ON CONFLICT(guild_id) DO UPDATE SET
                 revision=openskill_rating_revisions.revision+1,
                 updated_at=CURRENT_TIMESTAMP",
            [guild_id],
        )?;
        let cleared = transaction.execute(
            "DELETE FROM openskill_replay_jobs WHERE guild_id=?1",
            [guild_id],
        )?;
        if cleared != 1 {
            return Err(MatchCorrectionError::Invariant(
                "OpenSkill replay job changed before commit",
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn current_winner(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<MatchSide, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let value: i64 = connection
            .query_row(
                "SELECT winning_team FROM matches WHERE match_id=?1 AND guild_id=?2",
                params![match_id, guild_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(MatchCorrectionError::MatchNotFound(match_id))?;
        MatchSide::try_from(value)
    }

    pub fn corrections(&self, match_id: i64) -> Result<Vec<CorrectionAudit>, MatchCorrectionError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT correction_id,old_winning_team,new_winning_team,corrected_by
             FROM match_corrections WHERE match_id=?1
             ORDER BY corrected_at,correction_id",
        )?;
        let rows = statement.query_map([match_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (correction_id, old_winning_team, new_winning_team, corrected_by) = row?;
            Ok(CorrectionAudit {
                correction_id,
                old_winning_team: MatchSide::try_from(old_winning_team)?,
                new_winning_team: MatchSide::try_from(new_winning_team)?,
                corrected_by,
            })
        })
        .collect()
    }

    pub fn rating_snapshots(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<RatingSnapshot>, MatchCorrectionError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id,rating,rating_before,rd_before,volatility_before,
                    os_mu_before,os_sigma_before,streak_multiplier_per_game,
                    streak_threshold,base_rating_delta_multiplier,
                    low_priority_gain_multiplier
             FROM rating_history WHERE match_id=?1 AND guild_id=?2
             ORDER BY discord_id",
        )?;
        statement
            .query_map(params![match_id, guild_id], |row| {
                Ok(RatingSnapshot {
                    discord_id: row.get(0)?,
                    rating: row.get(1)?,
                    rating_before: row.get(2)?,
                    rd_before: row.get(3)?,
                    volatility_before: row.get(4)?,
                    os_mu_before: row.get(5)?,
                    os_sigma_before: row.get(6)?,
                    streak_multiplier_per_game: row.get(7)?,
                    streak_threshold: row.get(8)?,
                    base_rating_delta_multiplier: row.get(9)?,
                    low_priority_gain_multiplier: row.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

impl MatchCorrectionPersistence for MatchCorrectionRepository {
    fn update_participant_bonus_jc(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        bonus_by_player: &BTreeMap<i64, i64>,
        win_bonus_by_player: &BTreeMap<i64, i64>,
    ) -> Result<(), MatchCorrectionError> {
        MatchCorrectionRepository::update_participant_bonus_jc(
            self,
            match_id,
            guild_id,
            bonus_by_player,
            win_bonus_by_player,
        )
    }

    fn claim_match_bonuses_paid(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchCorrectionError> {
        MatchCorrectionRepository::claim_match_bonuses_paid(self, match_id, guild_id)
    }

    fn release_match_bonuses_claim(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, MatchCorrectionError> {
        MatchCorrectionRepository::release_match_bonuses_claim(self, match_id, guild_id)
    }

    fn apply_win_bonus_reversal_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        debits_by_player: &BTreeMap<i64, i64>,
    ) -> Result<(), MatchCorrectionError> {
        self.reverse_win_bonuses_atomic(match_id, guild_id, debits_by_player)
    }

    fn get_win_bonus_credited_ids(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeSet<i64>, MatchCorrectionError> {
        self.win_bonus_credited_ids(match_id, guild_id)
    }

    fn claim_match_correction(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
        owner_token: &str,
        now: i64,
        stale_after_seconds: i64,
    ) -> Result<CorrectionClaim, MatchCorrectionError> {
        MatchCorrectionRepository::claim_match_correction(
            self,
            match_id,
            guild_id,
            new_winning_team,
            owner_token,
            now,
            stale_after_seconds,
        )
    }

    fn release_match_correction_claim(
        &self,
        match_id: i64,
        owner_token: &str,
    ) -> Result<bool, MatchCorrectionError> {
        MatchCorrectionRepository::release_match_correction_claim(self, match_id, owner_token)
    }

    fn complete_match_correction_claim(
        &self,
        match_id: i64,
        owner_token: &str,
    ) -> Result<bool, MatchCorrectionError> {
        MatchCorrectionRepository::complete_match_correction_claim(self, match_id, owner_token)
    }
}

impl BetCorrectionPersistence for MatchCorrectionRepository {
    fn get_settled_bets_for_match(
        &self,
        match_id: i64,
    ) -> Result<Vec<SettledBetRecord>, MatchCorrectionError> {
        MatchCorrectionRepository::get_settled_bets_for_match(self, match_id)
    }

    fn settle_bet_correction_atomic(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_team: MatchSide,
        options: BetCorrectionOptions<'_>,
    ) -> Result<BetCorrectionResult, MatchCorrectionError> {
        MatchCorrectionRepository::settle_bet_correction_atomic(
            self,
            match_id,
            guild_id,
            new_winning_team,
            options,
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct ReplayPlayer {
    discord_id: i64,
    initial_mmr: Option<i64>,
}

#[derive(Clone, Debug)]
struct ReplayMatch {
    match_id: i64,
    winning_team: MatchSide,
    match_date: Value,
    match_at: Option<f64>,
    team1_players: Option<String>,
    team2_players: Option<String>,
    streak_rate: Option<f64>,
    streak_threshold: Option<i64>,
}

#[derive(Clone, Debug)]
struct ReplayParticipant {
    discord_id: i64,
    team_number: Option<i64>,
    side: Option<String>,
    fantasy_points: Option<f64>,
    low_priority_gain_multiplier: f64,
}

impl ReplayParticipant {
    fn team(&self) -> Option<MatchSide> {
        match self.team_number {
            Some(1) => Some(MatchSide::Radiant),
            Some(2) => Some(MatchSide::Dire),
            _ => match self.side.as_deref() {
                Some("radiant") => Some(MatchSide::Radiant),
                Some("dire") => Some(MatchSide::Dire),
                _ => None,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct ReplayEvent {
    event_id: i64,
    discord_id: i64,
    event_type: String,
    value: f64,
    event_at: Option<f64>,
}

#[derive(Clone, Debug)]
struct ReplayHistoryRow {
    match_id: i64,
    match_date: Value,
    discord_id: i64,
    team_number: i64,
    won: bool,
    before: OpenSkillRating,
    after: OpenSkillRating,
    fantasy_weight: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct ReplayPrediction {
    match_id: i64,
    raw_probability: f64,
    calibrated_probability: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct ExistingReplayHistory {
    os_mu_before: Option<f64>,
    os_mu_after: Option<f64>,
    os_sigma_before: Option<f64>,
    os_sigma_after: Option<f64>,
    fantasy_weight: Option<f64>,
    algorithm_version: Option<i64>,
    algorithm_fingerprint: Option<String>,
    team_number: Option<i64>,
    won: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
struct ExistingReplayPrediction {
    calibrated_probability: Option<f64>,
    raw_probability: Option<f64>,
    algorithm_version: Option<i64>,
    algorithm_fingerprint: Option<String>,
}

fn load_replay_players(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<ReplayPlayer>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT discord_id,initial_mmr FROM players
         WHERE guild_id=?1 ORDER BY discord_id",
    )?;
    statement
        .query_map([guild_id], |row| {
            Ok(ReplayPlayer {
                discord_id: row.get(0)?,
                initial_mmr: row.get(1)?,
            })
        })?
        .collect()
}

fn load_replay_matches(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<ReplayMatch>, MatchCorrectionError> {
    let mut statement = connection.prepare(
        "SELECT m.match_id,m.winning_team,m.match_date,julianday(m.match_date),
                m.team1_players,m.team2_players,
                (
                    SELECT rh.streak_multiplier_per_game
                    FROM rating_history rh
                    WHERE rh.guild_id=m.guild_id AND rh.match_id=m.match_id
                    ORDER BY rh.id LIMIT 1
                ),
                (
                    SELECT rh.streak_threshold
                    FROM rating_history rh
                    WHERE rh.guild_id=m.guild_id AND rh.match_id=m.match_id
                    ORDER BY rh.id LIMIT 1
                )
         FROM matches m
         WHERE m.guild_id=?1 AND m.winning_team IN (1,2)
         ORDER BY julianday(m.match_date) IS NULL,julianday(m.match_date),m.match_id",
    )?;
    let rows = statement
        .query_map([guild_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Value>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(
                match_id,
                winning_team,
                match_date,
                match_at,
                team1_players,
                team2_players,
                streak_rate,
                streak_threshold,
            )| {
                Ok(ReplayMatch {
                    match_id,
                    winning_team: MatchSide::try_from(winning_team)?,
                    match_date,
                    match_at,
                    team1_players,
                    team2_players,
                    streak_rate,
                    streak_threshold,
                })
            },
        )
        .collect()
}

fn load_replay_participants(
    connection: &Connection,
    guild_id: i64,
) -> Result<BTreeMap<i64, Vec<ReplayParticipant>>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT mp.match_id,mp.discord_id,mp.team_number,mp.side,mp.fantasy_points,
                COALESCE((
                    SELECT rh.low_priority_gain_multiplier
                    FROM rating_history rh
                    WHERE rh.guild_id=mp.guild_id AND rh.match_id=mp.match_id
                      AND rh.discord_id=mp.discord_id
                    ORDER BY rh.id LIMIT 1
                ),1.0)
         FROM match_participants mp
         JOIN matches m ON m.match_id=mp.match_id AND m.guild_id=mp.guild_id
         WHERE m.guild_id=?1 AND m.winning_team IN (1,2)
         ORDER BY mp.match_id,mp.discord_id",
    )?;
    let rows = statement
        .query_map([guild_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                ReplayParticipant {
                    discord_id: row.get(1)?,
                    team_number: row.get(2)?,
                    side: row.get(3)?,
                    fantasy_points: row.get(4)?,
                    low_priority_gain_multiplier: row.get(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut participants = BTreeMap::<i64, Vec<ReplayParticipant>>::new();
    for (match_id, participant) in rows {
        participants.entry(match_id).or_default().push(participant);
    }
    Ok(participants)
}

fn load_replay_events(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<ReplayEvent>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT event_id,discord_id,event_type,value,julianday(event_at)
         FROM openskill_rating_events WHERE guild_id=?1
         ORDER BY julianday(event_at) IS NULL,julianday(event_at),event_id",
    )?;
    statement
        .query_map([guild_id], |row| {
            Ok(ReplayEvent {
                event_id: row.get(0)?,
                discord_id: row.get(1)?,
                event_type: row.get(2)?,
                value: row.get(3)?,
                event_at: row.get(4)?,
            })
        })?
        .collect()
}

fn apply_replay_events_through(
    events: &[ReplayEvent],
    offset: &mut usize,
    through: Option<f64>,
    current: &mut BTreeMap<i64, OpenSkillRating>,
    players_touched: &mut BTreeSet<i64>,
    errors: &mut Vec<String>,
) {
    while let Some(event) = events.get(*offset) {
        if let Some(through) = through {
            match event.event_at {
                Some(event_at) if event_at <= through => {}
                _ => break,
            }
        }
        *offset += 1;
        let Some(rating) = current.get_mut(&event.discord_id) else {
            errors.push(format!(
                "OpenSkill event {}: unknown player {}",
                event.event_id, event.discord_id
            ));
            continue;
        };
        if !event.value.is_finite() {
            errors.push(format!(
                "OpenSkill event {}: non-finite value",
                event.event_id
            ));
            continue;
        }
        match event.event_type.as_str() {
            "set_mu" => rating.mu = event.value,
            "set_sigma" => {
                rating.sigma = event.value.clamp(0.0, CamaOpenSkillSystem::DEFAULT_SIGMA);
            }
            "add_sigma" => {
                rating.sigma =
                    (rating.sigma + event.value).clamp(0.0, CamaOpenSkillSystem::DEFAULT_SIGMA);
            }
            event_type => {
                errors.push(format!(
                    "OpenSkill event {}: unknown type {event_type:?}",
                    event.event_id
                ));
                continue;
            }
        }
        players_touched.insert(event.discord_id);
    }
}

fn replay_team_ids(
    replay_match: &ReplayMatch,
    participants: &[ReplayParticipant],
) -> Result<(Vec<i64>, Vec<i64>), String> {
    let participant_radiant = participants
        .iter()
        .filter(|participant| participant.team() == Some(MatchSide::Radiant))
        .map(|participant| participant.discord_id)
        .collect::<Vec<_>>();
    let participant_dire = participants
        .iter()
        .filter(|participant| participant.team() == Some(MatchSide::Dire))
        .map(|participant| participant.discord_id)
        .collect::<Vec<_>>();
    let (radiant, dire) = if participant_radiant.len() == 5 && participant_dire.len() == 5 {
        (participant_radiant, participant_dire)
    } else {
        (
            parse_json_id_array(replay_match.team1_players.as_deref()).unwrap_or_default(),
            parse_json_id_array(replay_match.team2_players.as_deref()).unwrap_or_default(),
        )
    };
    let unique = radiant
        .iter()
        .chain(&dire)
        .copied()
        .collect::<BTreeSet<_>>();
    if radiant.len() != 5 || dire.len() != 5 || unique.len() != 10 {
        return Err(format!(
            "Match {}: invalid team sizes {}/{}",
            replay_match.match_id,
            radiant.len(),
            dire.len()
        ));
    }
    Ok((radiant, dire))
}

fn parse_json_id_array(value: Option<&str>) -> Option<Vec<i64>> {
    let value = value?.trim();
    let inner = value.strip_prefix('[')?.strip_suffix(']')?.trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|item| item.trim().trim_matches('"').parse::<i64>().ok())
        .collect()
}

fn persist_openskill_replay(
    transaction: &Transaction<'_>,
    guild_id: i64,
    history_rows: &[ReplayHistoryRow],
    prediction_rows: &[ReplayPrediction],
    current: &BTreeMap<i64, OpenSkillRating>,
    fingerprint: &str,
) -> Result<(), rusqlite::Error> {
    let existing_history = {
        let mut statement = transaction.prepare(
            "SELECT match_id,discord_id,os_mu_before,os_mu_after,
                    os_sigma_before,os_sigma_after,fantasy_weight,
                    os_algorithm_version,os_algorithm_fingerprint,
                    team_number,won
               FROM rating_history WHERE guild_id=?1",
        )?;
        statement
            .query_map([guild_id], |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?),
                    ExistingReplayHistory {
                        os_mu_before: row.get(2)?,
                        os_mu_after: row.get(3)?,
                        os_sigma_before: row.get(4)?,
                        os_sigma_after: row.get(5)?,
                        fantasy_weight: row.get(6)?,
                        algorithm_version: row.get(7)?,
                        algorithm_fingerprint: row.get(8)?,
                        team_number: row.get(9)?,
                        won: row.get(10)?,
                    },
                ))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?
    };
    for history in history_rows {
        let existing = existing_history.get(&(history.match_id, history.discord_id));
        if existing.is_none() {
            transaction.execute(
                "INSERT INTO rating_history (
                     discord_id,guild_id,match_id,timestamp,team_number,won,
                     os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,
                     fantasy_weight,os_algorithm_version,os_algorithm_fingerprint
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    history.discord_id,
                    guild_id,
                    history.match_id,
                    &history.match_date,
                    history.team_number,
                    history.won,
                    history.before.mu,
                    history.after.mu,
                    history.before.sigma,
                    history.after.sigma,
                    history.fantasy_weight,
                    OPENSKILL_REPLAY_ALGORITHM_VERSION,
                    fingerprint
                ],
            )?;
            continue;
        }
        let existing = existing.expect("checked replay history presence");
        let changed = existing.os_mu_before != Some(history.before.mu)
            || existing.os_mu_after != Some(history.after.mu)
            || existing.os_sigma_before != Some(history.before.sigma)
            || existing.os_sigma_after != Some(history.after.sigma)
            || existing.fantasy_weight != history.fantasy_weight
            || existing.algorithm_version != Some(OPENSKILL_REPLAY_ALGORITHM_VERSION)
            || existing.algorithm_fingerprint.as_deref() != Some(fingerprint)
            || (existing.team_number.is_none() && history.team_number != 0)
            || existing.won.is_none();
        if changed {
            transaction.execute(
                "UPDATE rating_history
                 SET os_mu_before=?1,os_mu_after=?2,os_sigma_before=?3,
                     os_sigma_after=?4,fantasy_weight=?5,
                     os_algorithm_version=?6,os_algorithm_fingerprint=?7,
                     team_number=COALESCE(team_number,?8),won=COALESCE(won,?9)
                 WHERE guild_id=?10 AND match_id=?11 AND discord_id=?12",
                params![
                    history.before.mu,
                    history.after.mu,
                    history.before.sigma,
                    history.after.sigma,
                    history.fantasy_weight,
                    OPENSKILL_REPLAY_ALGORITHM_VERSION,
                    fingerprint,
                    history.team_number,
                    history.won,
                    guild_id,
                    history.match_id,
                    history.discord_id
                ],
            )?;
        }
    }
    let existing_predictions = {
        let mut statement = transaction.prepare(
            "SELECT p.match_id,p.openskill_radiant_win_prob,
                    p.openskill_raw_radiant_win_prob,
                    p.openskill_algorithm_version,
                    p.openskill_algorithm_fingerprint
               FROM match_predictions AS p
               JOIN matches AS m ON m.match_id=p.match_id
              WHERE m.guild_id=?1",
        )?;
        statement
            .query_map([guild_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    ExistingReplayPrediction {
                        calibrated_probability: row.get(1)?,
                        raw_probability: row.get(2)?,
                        algorithm_version: row.get(3)?,
                        algorithm_fingerprint: row.get(4)?,
                    },
                ))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?
    };
    for prediction in prediction_rows {
        let unchanged = existing_predictions
            .get(&prediction.match_id)
            .is_some_and(|existing| {
                existing.calibrated_probability == Some(prediction.calibrated_probability)
                    && existing.raw_probability == Some(prediction.raw_probability)
                    && existing.algorithm_version == Some(OPENSKILL_REPLAY_ALGORITHM_VERSION)
                    && existing.algorithm_fingerprint.as_deref() == Some(fingerprint)
            });
        if unchanged {
            continue;
        }
        transaction.execute(
            "INSERT INTO match_predictions (
                 match_id,openskill_radiant_win_prob,
                 openskill_raw_radiant_win_prob,openskill_algorithm_version,
                 openskill_algorithm_fingerprint
             ) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(match_id) DO UPDATE SET
                 openskill_radiant_win_prob=excluded.openskill_radiant_win_prob,
                 openskill_raw_radiant_win_prob=excluded.openskill_raw_radiant_win_prob,
                 openskill_algorithm_version=excluded.openskill_algorithm_version,
                 openskill_algorithm_fingerprint=excluded.openskill_algorithm_fingerprint",
            params![
                prediction.match_id,
                prediction.calibrated_probability,
                prediction.raw_probability,
                OPENSKILL_REPLAY_ALGORITHM_VERSION,
                fingerprint
            ],
        )?;
    }
    for (&discord_id, rating) in current {
        transaction.execute(
            "UPDATE players
             SET os_mu=?1,os_sigma=?2,os_rating_version=?3,
                 os_algorithm_fingerprint=?4,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?5 AND guild_id=?6",
            params![
                rating.mu,
                rating.sigma,
                OPENSKILL_REPLAY_ALGORITHM_VERSION,
                fingerprint,
                discord_id,
                guild_id
            ],
        )?;
    }
    Ok(())
}

fn matches_current_or_requested(current: MatchSide, old: MatchSide, requested: MatchSide) -> bool {
    current == old || current == requested
}

fn optional_real(value: Option<f64>) -> RecordedValue<'static> {
    match value {
        Some(value) => RecordedValue::Real(value),
        None => RecordedValue::Null,
    }
}

fn optional_integer(value: Option<i64>) -> RecordedValue<'static> {
    match value {
        Some(value) => RecordedValue::Integer(value),
        None => RecordedValue::Null,
    }
}

fn replay_streak_multipliers(
    rating_system: &CamaRatingSystem,
    all_ids: &[i64],
    radiant_ids: &[i64],
    winning_team: MatchSide,
    recent_outcomes: &BTreeMap<i64, Vec<bool>>,
    rate: f64,
    threshold: i64,
) -> Result<BTreeMap<u64, f64>, i64> {
    all_ids
        .iter()
        .map(|&discord_id| {
            let openskill_id = u64::try_from(discord_id).map_err(|_| discord_id)?;
            let won = side_for_player(discord_id, radiant_ids) == winning_team;
            let adjustment = rating_system.calculate_streak_multiplier(
                recent_outcomes
                    .get(&discord_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                won,
                Some(rate),
                Some(threshold),
            );
            Ok((openskill_id, adjustment.multiplier))
        })
        .collect()
}

fn record_replay_outcome(
    recent_outcomes: &mut BTreeMap<i64, Vec<bool>>,
    discord_id: i64,
    won: bool,
) {
    let outcomes = recent_outcomes.entry(discord_id).or_default();
    outcomes.insert(0, won);
    outcomes.truncate(RECENT_OUTCOME_LIMIT);
}

fn side_for_player(discord_id: i64, radiant_ids: &[i64]) -> MatchSide {
    if radiant_ids.contains(&discord_id) {
        MatchSide::Radiant
    } else {
        MatchSide::Dire
    }
}

fn participant_teams_from_connection(
    connection: &Connection,
    match_id: i64,
    guild_id: i64,
) -> Result<(Vec<i64>, Vec<i64>), MatchCorrectionError> {
    let mut statement = connection.prepare(
        "SELECT discord_id,team_number,side FROM match_participants
         WHERE match_id=?1 AND guild_id=?2 ORDER BY discord_id",
    )?;
    let participants = statement
        .query_map(params![match_id, guild_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if participants.is_empty() {
        return Err(MatchCorrectionError::Invariant(
            "a correction requires persisted match participants",
        ));
    }
    let mut radiant = Vec::new();
    let mut dire = Vec::new();
    for (discord_id, team_number, side) in participants {
        match side.as_deref() {
            Some("radiant") => radiant.push(discord_id),
            Some("dire") => dire.push(discord_id),
            _ => match team_number {
                Some(1) => radiant.push(discord_id),
                Some(2) => dire.push(discord_id),
                _ => {
                    return Err(MatchCorrectionError::Invariant(
                        "every participant requires a radiant or dire side",
                    ));
                }
            },
        }
    }
    if radiant.is_empty() || dire.is_empty() {
        return Err(MatchCorrectionError::Invariant(
            "a correction requires both persisted teams",
        ));
    }
    Ok((radiant, dire))
}

fn participant_fantasy_points(
    connection: &Connection,
    match_id: i64,
    guild_id: i64,
) -> Result<BTreeMap<i64, Option<f64>>, MatchCorrectionError> {
    let mut statement = connection.prepare(
        "SELECT discord_id,fantasy_points FROM match_participants
         WHERE match_id=?1 AND guild_id=?2",
    )?;
    statement
        .query_map(params![match_id, guild_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(Into::into)
}

fn verify_owned_pending_claim(
    transaction: &Transaction<'_>,
    request: &CoreCorrectionRequest<'_>,
    guild_id: i64,
) -> Result<(), MatchCorrectionError> {
    let valid = transaction
        .query_row(
            "SELECT 1 FROM match_correction_claims
             WHERE match_id=?1 AND guild_id=?2 AND old_winning_team=?3
               AND new_winning_team=?4 AND state='pending' AND owner_token=?5",
            params![
                request.match_id,
                guild_id,
                request.old_winning_team.number(),
                request.new_winning_team.number(),
                request.owner_token
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if valid {
        Ok(())
    } else {
        Err(MatchCorrectionError::ClaimNotOwned(request.match_id))
    }
}

fn participant_teams(
    transaction: &Transaction<'_>,
    match_id: i64,
    guild_id: i64,
) -> Result<(Vec<i64>, Vec<i64>), MatchCorrectionError> {
    participant_teams_from_connection(transaction, match_id, guild_id)
}

fn canonical_pair(first: i64, second: i64) -> (i64, i64) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

fn apply_pairing_win_deltas(
    transaction: &Transaction<'_>,
    guild_id: i64,
    radiant_ids: &[i64],
    dire_ids: &[i64],
    old_winner: MatchSide,
    new_winner: MatchSide,
) -> Result<(), rusqlite::Error> {
    for (team, players) in [
        (MatchSide::Radiant, radiant_ids),
        (MatchSide::Dire, dire_ids),
    ] {
        let delta = i64::from(team == new_winner) - i64::from(team == old_winner);
        for (index, &first) in players.iter().enumerate() {
            for &second in &players[index + 1..] {
                let (player1, player2) = canonical_pair(first, second);
                transaction.execute(
                    "UPDATE player_pairings
                     SET wins_together=wins_together+?1,updated_at=CURRENT_TIMESTAMP
                     WHERE guild_id=?2 AND player1_id=?3 AND player2_id=?4",
                    params![delta, guild_id, player1, player2],
                )?;
            }
        }
    }
    for &radiant in radiant_ids {
        for &dire in dire_ids {
            let (player1, player2) = canonical_pair(radiant, dire);
            let canonical_first_is_radiant = player1 == radiant;
            let player1_old_won = if canonical_first_is_radiant {
                old_winner == MatchSide::Radiant
            } else {
                old_winner == MatchSide::Dire
            };
            let player1_new_won = if canonical_first_is_radiant {
                new_winner == MatchSide::Radiant
            } else {
                new_winner == MatchSide::Dire
            };
            let delta = i64::from(player1_new_won) - i64::from(player1_old_won);
            transaction.execute(
                "UPDATE player_pairings
                 SET player1_wins_against=player1_wins_against+?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE guild_id=?2 AND player1_id=?3 AND player2_id=?4",
                params![delta, guild_id, player1, player2],
            )?;
        }
    }
    Ok(())
}

fn install_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    related_type: &str,
    related_id: i64,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id,source,related_type,related_id,reason
         ) VALUES (1,?1,?2,?3,?4)",
        params![source, related_type, related_id.to_string(), reason],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn win_bonus_ledger_marker_exists(
    transaction: &Transaction<'_>,
    match_id: i64,
    guild_id: i64,
    discord_id: i64,
) -> Result<bool, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT 1 FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_type='player' AND account_id=?2
               AND related_type='match_win_bonus' AND related_id=?3
             LIMIT 1",
            params![guild_id, discord_id, match_id.to_string()],
            |_| Ok(()),
        )
        .optional()
        .map(|marker| marker.is_some())
}

#[derive(Clone, Debug)]
struct Bet {
    bet_id: i64,
    discord_id: i64,
    team: MatchSide,
    effective_bet: i64,
    payout: Option<i64>,
}

fn load_bets(connection: &Connection, match_id: i64) -> Result<Vec<Bet>, MatchCorrectionError> {
    let mut statement = connection.prepare(
        "SELECT bet_id,discord_id,team_bet_on,
                amount*COALESCE(leverage,1),payout
         FROM bets WHERE match_id=?1 ORDER BY bet_time,bet_id",
    )?;
    statement
        .query_map([match_id], |row| {
            let team = row.get::<_, String>(2)?;
            let team = if team == "radiant" {
                MatchSide::Radiant
            } else {
                MatchSide::Dire
            };
            Ok(Bet {
                bet_id: row.get(0)?,
                discord_id: row.get(1)?,
                team,
                effective_bet: row.get(3)?,
                payout: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn player_groups<'a>(bets: &[&'a Bet]) -> Vec<(i64, Vec<&'a Bet>)> {
    let mut groups: Vec<(i64, Vec<&Bet>)> = Vec::new();
    for &bet in bets {
        if let Some((_, player_bets)) = groups
            .iter_mut()
            .find(|(discord_id, _)| *discord_id == bet.discord_id)
        {
            player_bets.push(bet);
        } else {
            groups.push((bet.discord_id, vec![bet]));
        }
    }
    groups
}

fn compute_new_payouts(
    all_bets: &[Bet],
    old_winners: &[&Bet],
    new_winners: &[&Bet],
    pool_mode: bool,
    house_payout_multiplier: f64,
) -> (BTreeMap<i64, i64>, BTreeMap<i64, i64>) {
    let settled_total = old_winners
        .iter()
        .map(|bet| bet.payout.unwrap_or(0))
        .sum::<i64>();
    let groups = player_groups(new_winners);
    let mut user_payouts = BTreeMap::new();
    if pool_mode {
        let winner_pool = new_winners.iter().map(|bet| bet.effective_bet).sum::<i64>();
        if winner_pool == 0 {
            return (user_payouts, BTreeMap::new());
        }
        if settled_total > 0 {
            let mut allocated = 0;
            let last = groups.len().saturating_sub(1);
            for (index, (discord_id, bets)) in groups.iter().enumerate() {
                let payout = if index == last {
                    settled_total - allocated
                } else {
                    let effective = bets.iter().map(|bet| bet.effective_bet).sum::<i64>();
                    let payout =
                        ((effective as f64 / winner_pool as f64) * settled_total as f64) as i64;
                    allocated += payout;
                    payout
                };
                user_payouts.insert(*discord_id, payout);
            }
        } else {
            let total_pool = all_bets.iter().map(|bet| bet.effective_bet).sum::<i64>();
            for (discord_id, bets) in &groups {
                let effective = bets.iter().map(|bet| bet.effective_bet).sum::<i64>();
                let payout =
                    ((effective as f64 / winner_pool as f64) * total_pool as f64).ceil() as i64;
                user_payouts.insert(*discord_id, payout);
            }
        }
    } else {
        let old_base = old_winners
            .iter()
            .map(|bet| (bet.effective_bet as f64 * (1.0 + house_payout_multiplier)) as i64)
            .sum::<i64>();
        let extra_pot = (settled_total - old_base).max(0);
        let winner_pool = new_winners.iter().map(|bet| bet.effective_bet).sum::<i64>();
        let mut allocated_extra = 0;
        let last = groups.len().saturating_sub(1);
        let mut payout_updates = BTreeMap::new();
        for (index, (discord_id, bets)) in groups.iter().enumerate() {
            let effective = bets.iter().map(|bet| bet.effective_bet).sum::<i64>();
            let extra = if index == last {
                extra_pot - allocated_extra
            } else if winner_pool > 0 {
                let share = ((effective as f64 / winner_pool as f64) * extra_pot as f64) as i64;
                allocated_extra += share;
                share
            } else {
                0
            };
            let last_bet = bets.len().saturating_sub(1);
            let mut user_total = 0;
            for (bet_index, bet) in bets.iter().enumerate() {
                let mut payout =
                    (bet.effective_bet as f64 * (1.0 + house_payout_multiplier)) as i64;
                if bet_index == last_bet {
                    payout += extra;
                }
                user_total += payout;
                payout_updates.insert(bet.bet_id, payout);
            }
            user_payouts.insert(*discord_id, user_total);
        }
        return (user_payouts, payout_updates);
    }

    let mut payout_updates = BTreeMap::new();
    for (discord_id, bets) in groups {
        let user_payout = user_payouts.get(&discord_id).copied().unwrap_or(0);
        let bet_sum = bets.iter().map(|bet| bet.effective_bet).sum::<i64>();
        let mut allocated = 0;
        let last = bets.len().saturating_sub(1);
        for (index, bet) in bets.iter().enumerate() {
            let payout = if index == last {
                user_payout - allocated
            } else if bet_sum > 0 {
                let share =
                    ((bet.effective_bet as f64 / bet_sum as f64) * user_payout as f64) as i64;
                allocated += share;
                share
            } else {
                0
            };
            payout_updates.insert(bet.bet_id, payout);
        }
    }
    (user_payouts, payout_updates)
}

/// The withholdings persisted for one player's settled bets.  Reversing that
/// settlement has to refund every one of them, not just the vanity share.
#[derive(Clone, Copy, Debug)]
struct SettlementTaxes {
    vanity_tax: i64,
    low_priority_tax: i64,
}

impl SettlementTaxes {
    const fn total(self) -> i64 {
        self.vanity_tax + self.low_priority_tax
    }
}

fn load_settlement_taxes(
    transaction: &Transaction<'_>,
    match_id: i64,
    guild_id: i64,
) -> Result<BTreeMap<i64, SettlementTaxes>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT discord_id,vanity_tax,low_priority_tax FROM bet_settlement_taxes
         WHERE match_id=?1 AND guild_id=?2",
    )?;
    statement
        .query_map(params![match_id, guild_id], |row| {
            Ok((
                row.get(0)?,
                SettlementTaxes {
                    vanity_tax: row.get(1)?,
                    low_priority_tax: row.get(2)?,
                },
            ))
        })?
        .collect()
}

fn total_stakes_by_player(bets: &[Bet]) -> BTreeMap<i64, i64> {
    let mut totals = BTreeMap::new();
    for bet in bets {
        *totals.entry(bet.discord_id).or_insert(0) += bet.effective_bet;
    }
    totals
}

#[cfg(test)]
#[path = "match_correction/tests.rs"]
mod tests;
