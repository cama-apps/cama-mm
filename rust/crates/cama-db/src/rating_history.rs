//! Python-compatible access to match outcome and rating-history windows.
//!
//! Python remains the schema-migration authority during the side-by-side
//! rewrite. This repository therefore opens an existing database without the
//! `CREATE` flag, uses the same five-second busy timeout, and explicitly keeps
//! SQLite foreign-key enforcement disabled. The outcome queries intentionally
//! order by match chronology rather than `rating_history.id`: an OpenSkill
//! replay can backfill an old match after newer history rows already exist.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use cama_domain::openskill::CamaOpenSkillSystem;
use chrono::Utc;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

const OPENSKILL_ALGORITHM_VERSION: i64 = 5;

/// The subset of a Python `rating_history` write used by the rating policy.
///
/// Optional values are stored as SQL `NULL`, preserving the distinction
/// between an absent streak measurement and a measured value of zero.
#[derive(Clone, Debug, PartialEq)]
pub struct NewRatingHistoryEntry {
    pub discord_id: i64,
    pub guild_id: i64,
    pub rating: f64,
    pub match_id: Option<i64>,
    pub rating_before: Option<f64>,
    pub rd_before: Option<f64>,
    pub rd_after: Option<f64>,
    pub volatility_before: Option<f64>,
    pub volatility_after: Option<f64>,
    pub expected_team_win_prob: Option<f64>,
    pub team_number: Option<i64>,
    pub won: Option<bool>,
    pub os_mu_before: Option<f64>,
    pub os_mu_after: Option<f64>,
    pub os_sigma_before: Option<f64>,
    pub os_sigma_after: Option<f64>,
    pub fantasy_weight: Option<f64>,
    pub os_algorithm_version: Option<i64>,
    pub os_algorithm_fingerprint: Option<String>,
    pub streak_length: Option<i64>,
    pub streak_multiplier: Option<f64>,
}

impl NewRatingHistoryEntry {
    /// Construct a sparse row like Python's `add_rating_history` defaults.
    #[must_use]
    pub fn new(discord_id: i64, guild_id: i64, rating: f64) -> Self {
        Self {
            discord_id,
            guild_id,
            rating,
            match_id: None,
            rating_before: None,
            rd_before: None,
            rd_after: None,
            volatility_before: None,
            volatility_after: None,
            expected_team_win_prob: None,
            team_number: None,
            won: None,
            os_mu_before: None,
            os_mu_after: None,
            os_sigma_before: None,
            os_sigma_after: None,
            fantasy_weight: None,
            os_algorithm_version: None,
            os_algorithm_fingerprint: None,
            streak_length: None,
            streak_multiplier: None,
        }
    }
}

/// Projection returned by Python's `get_rating_history` method.
#[derive(Clone, Debug, PartialEq)]
pub struct RatingHistoryEntry {
    pub rating: f64,
    pub match_id: Option<i64>,
    /// SQLite permits legacy timestamps to have any storage class.
    pub timestamp: Value,
    pub streak_length: Option<i64>,
    pub streak_multiplier: Option<f64>,
}

/// Nullable rating values needed by image renderers.
///
/// The profile chart deliberately keeps rows where Glicko is absent but an
/// OpenSkill snapshot exists.  That is a real state in the migrated schema,
/// so it cannot be represented by [`RatingHistoryEntry`], whose `rating`
/// field preserves the older Python repository contract as non-null.
#[derive(Clone, Debug, PartialEq)]
pub struct RatingChartEntry {
    pub rating: Option<f64>,
    pub openskill_mu: Option<f64>,
    pub won: Option<bool>,
}

/// The per-match rating snapshot consumed by post-match debrief selection.
///
/// This keeps the Match runtime on the existing `rating_history` authority
/// while exposing both Glicko and OpenSkill deltas needed by Python's
/// `_get_notable_winner` policy.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchRatingHistoryEntry {
    pub discord_id: i64,
    pub rating_before: Option<f64>,
    pub rating_after: Option<f64>,
    pub openskill_mu_before: Option<f64>,
    pub openskill_mu_after: Option<f64>,
    pub expected_team_win_prob: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecalibrationState {
    pub discord_id: i64,
    pub guild_id: i64,
    pub last_recalibration_at: Option<i64>,
    pub total_recalibrations: i64,
    pub rating_at_recalibration: Option<f64>,
}

#[derive(Debug, Error)]
pub enum RecalibrationRepositoryError {
    #[error("ON_COOLDOWN:{remaining_seconds}")]
    OnCooldown { remaining_seconds: i64 },
    #[error("recalibration SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecalibrationRequest {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub now: i64,
    pub cooldown_seconds: i64,
    pub rating: f64,
    pub new_rd: f64,
    pub new_volatility: f64,
    pub new_os_sigma: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecalibrationStateUpsert {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub last_recalibration_at: Option<i64>,
    pub total_recalibrations: Option<i64>,
    pub rating_at_recalibration: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecalibrationServiceConfig {
    pub cooldown_seconds: i64,
    pub initial_rd: f64,
    pub initial_volatility: f64,
    pub min_games: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecalibrationServiceState {
    pub discord_id: i64,
    pub last_recalibration_at: Option<i64>,
    pub total_recalibrations: i64,
    pub is_on_cooldown: bool,
    pub cooldown_ends_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecalibrationEligibility {
    NotRegistered,
    NoRating,
    InsufficientGames {
        games_played: i64,
        min_games: i64,
    },
    OnCooldown {
        cooldown_ends_at: i64,
    },
    Allowed {
        current_rating: f64,
        current_rd: f64,
        current_volatility: f64,
        games_played: i64,
        current_os_sigma: Option<f64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecalibrationSuccess {
    pub old_rating: f64,
    pub old_rd: f64,
    pub old_volatility: f64,
    pub new_rd: f64,
    pub new_volatility: f64,
    pub old_os_sigma: Option<f64>,
    pub new_os_sigma: Option<f64>,
    pub total_recalibrations: i64,
    pub cooldown_ends_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecalibrationExecution {
    Rejected(RecalibrationEligibility),
    Success(RecalibrationSuccess),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecalibrationCooldownReset {
    NotRegistered,
    NoHistory,
    Reset { total_recalibrations: i64 },
}

/// Concrete service used by command adapters to share eligibility, cooldown,
/// and rating-reset semantics with the Python implementation.
#[derive(Clone, Debug)]
pub struct RecalibrationService {
    repository: RatingHistoryRepository,
    config: RecalibrationServiceConfig,
}

/// Repository for the existing Python-owned `rating_history` table.
#[derive(Clone, Debug)]
pub struct RatingHistoryRepository {
    path: PathBuf,
}

impl RatingHistoryRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Convert Python's nullable guild convention to its persisted sentinel.
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

    pub fn get_recalibration_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RecalibrationState>, RecalibrationRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT discord_id, guild_id, last_recalibration_at,
                        COALESCE(total_recalibrations, 0), rating_at_recalibration
                 FROM recalibration_state
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| {
                    Ok(RecalibrationState {
                        discord_id: row.get(0)?,
                        guild_id: row.get(1)?,
                        last_recalibration_at: row.get(2)?,
                        total_recalibrations: row.get(3)?,
                        rating_at_recalibration: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Python-compatible partial upsert for admin/tests and state repair.
    /// Null fields preserve the corresponding existing value on conflict.
    pub fn upsert_recalibration_state(
        &self,
        request: RecalibrationStateUpsert,
    ) -> Result<(), RecalibrationRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        self.connection()?.execute(
            "INSERT INTO recalibration_state (
                 discord_id,guild_id,last_recalibration_at,total_recalibrations,
                 rating_at_recalibration,updated_at
             ) VALUES (?1,?2,?3,?4,?5,CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                 last_recalibration_at=COALESCE(
                     excluded.last_recalibration_at,
                     recalibration_state.last_recalibration_at
                 ),
                 total_recalibrations=COALESCE(
                     excluded.total_recalibrations,
                     recalibration_state.total_recalibrations
                 ),
                 rating_at_recalibration=COALESCE(
                     excluded.rating_at_recalibration,
                     recalibration_state.rating_at_recalibration
                 ),
                 updated_at=CURRENT_TIMESTAMP",
            params![
                request.discord_id,
                guild_id,
                request.last_recalibration_at,
                request.total_recalibrations,
                request.rating_at_recalibration,
            ],
        )?;
        Ok(())
    }

    /// Reset only the cooldown timestamp while preserving the durable count.
    pub fn reset_recalibration_cooldown(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, RecalibrationRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        Ok(self.connection()?.execute(
            "UPDATE recalibration_state
             SET last_recalibration_at=0,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )? != 0)
    }

    fn recalibration_player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RecalibrationPlayer>, RecalibrationRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        self.connection()?
            .query_row(
                "SELECT glicko_rating,glicko_rd,glicko_volatility,os_sigma,
                        COALESCE(wins,0)+COALESCE(losses,0)
                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| {
                    Ok(RecalibrationPlayer {
                        rating: row.get(0)?,
                        rd: row.get(1)?,
                        volatility: row.get(2)?,
                        os_sigma: row.get(3)?,
                        games_played: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Re-check cooldown, reset both rating systems' uncertainty, and advance
    /// the durable recalibration counter in one `BEGIN IMMEDIATE` transaction.
    pub fn execute_recalibration_atomic(
        &self,
        request: RecalibrationRequest,
    ) -> Result<i64, RecalibrationRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = transaction
            .query_row(
                "SELECT last_recalibration_at, COALESCE(total_recalibrations, 0)
                 FROM recalibration_state
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![request.discord_id, guild_id],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let (last_at, total) = state.unwrap_or((None, 0));
        if let Some(last_at) = last_at.filter(|last_at| *last_at != 0) {
            let elapsed = request.now.saturating_sub(last_at);
            if elapsed < request.cooldown_seconds {
                return Err(RecalibrationRepositoryError::OnCooldown {
                    remaining_seconds: request.cooldown_seconds.saturating_sub(elapsed),
                });
            }
        }

        let fingerprint = CamaOpenSkillSystem::default().algorithm_fingerprint();
        transaction.execute(
            "UPDATE players
             SET glicko_rating = ?1, glicko_rd = ?2, glicko_volatility = ?3,
                 os_sigma = COALESCE(?4, os_sigma),
                 os_rating_version = CASE
                     WHEN ?4 IS NULL THEN os_rating_version ELSE ?5
                 END,
                 os_algorithm_fingerprint = CASE
                     WHEN ?4 IS NULL THEN os_algorithm_fingerprint ELSE ?6
                 END,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?7 AND guild_id = ?8",
            params![
                request.rating,
                request.new_rd,
                request.new_volatility,
                request.new_os_sigma,
                OPENSKILL_ALGORITHM_VERSION,
                fingerprint,
                request.discord_id,
                guild_id
            ],
        )?;
        let new_total = total.saturating_add(1);
        transaction.execute(
            "INSERT INTO recalibration_state (
                 discord_id, guild_id, last_recalibration_at,
                 total_recalibrations, rating_at_recalibration, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 last_recalibration_at = excluded.last_recalibration_at,
                 total_recalibrations = excluded.total_recalibrations,
                 rating_at_recalibration = excluded.rating_at_recalibration,
                 updated_at = CURRENT_TIMESTAMP",
            params![
                request.discord_id,
                guild_id,
                request.now,
                new_total,
                request.rating
            ],
        )?;
        if let Some(new_os_sigma) = request.new_os_sigma {
            transaction.execute(
                "INSERT INTO openskill_rating_events (
                     guild_id,discord_id,event_type,value,event_at,reason,actor_id,
                     os_algorithm_version,os_algorithm_fingerprint
                 ) VALUES (?1,?2,'set_sigma',?3,?4,'player_recalibration',?2,?5,?6)",
                params![
                    guild_id,
                    request.discord_id,
                    new_os_sigma,
                    Utc::now().to_rfc3339(),
                    OPENSKILL_ALGORITHM_VERSION,
                    fingerprint,
                ],
            )?;
            increment_openskill_revision(&transaction, guild_id)?;
        }
        transaction.commit()?;
        Ok(new_total)
    }

    /// Insert one row using the same column set as Python's history writer.
    pub fn add_rating_history(
        &self,
        entry: &NewRatingHistoryEntry,
    ) -> Result<i64, rusqlite::Error> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO rating_history (
                discord_id,
                guild_id,
                rating,
                rating_before,
                rd_before,
                rd_after,
                volatility_before,
                volatility_after,
                expected_team_win_prob,
                team_number,
                won,
                match_id,
                os_mu_before,
                os_mu_after,
                os_sigma_before,
                os_sigma_after,
                fantasy_weight,
                os_algorithm_version,
                os_algorithm_fingerprint,
                streak_length,
                streak_multiplier
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21
            )",
            params![
                entry.discord_id,
                entry.guild_id,
                entry.rating,
                entry.rating_before,
                entry.rd_before,
                entry.rd_after,
                entry.volatility_before,
                entry.volatility_after,
                entry.expected_team_win_prob,
                entry.team_number,
                entry.won,
                entry.match_id,
                entry.os_mu_before,
                entry.os_mu_after,
                entry.os_sigma_before,
                entry.os_sigma_after,
                entry.fantasy_weight,
                entry.os_algorithm_version,
                entry.os_algorithm_fingerprint.as_deref(),
                entry.streak_length,
                entry.streak_multiplier,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Return rating snapshots in the same timestamp-descending order as Python.
    pub fn get_rating_history(
        &self,
        discord_id: i64,
        guild_id: i64,
        limit: i64,
    ) -> Result<Vec<RatingHistoryEntry>, rusqlite::Error> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT rating, match_id, timestamp, streak_length, streak_multiplier
             FROM rating_history
             WHERE discord_id = ?1 AND guild_id = ?2
             ORDER BY timestamp DESC
             LIMIT ?3",
        )?;
        statement
            .query_map(params![discord_id, guild_id, limit], |row| {
                Ok(RatingHistoryEntry {
                    rating: row.get(0)?,
                    match_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    streak_length: row.get(3)?,
                    streak_multiplier: row.get(4)?,
                })
            })?
            .collect()
    }

    /// Return the nullable Glicko/OpenSkill snapshots used by profile chart
    /// rendering, newest first like Python's `full_history` query.
    pub fn get_rating_chart_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<RatingChartEntry>, rusqlite::Error> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT rating, os_mu_after, won
             FROM rating_history
             WHERE discord_id = ?1 AND guild_id = ?2
             ORDER BY timestamp DESC
             LIMIT ?3",
        )?;
        statement
            .query_map(params![discord_id, guild_id, limit], |row| {
                Ok(RatingChartEntry {
                    rating: row.get(0)?,
                    openskill_mu: row.get(1)?,
                    won: row.get(2)?,
                })
            })?
            .collect()
    }

    /// Return the rating snapshots for one settled match in stable player
    /// order.  The Match provider uses this read only after the core match
    /// transaction commits, so a retry observes the same durable facts.
    pub fn get_match_rating_history(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<MatchRatingHistoryEntry>, rusqlite::Error> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, rating_before, rating, os_mu_before,
                    os_mu_after, expected_team_win_prob
             FROM rating_history
             WHERE match_id = ?1 AND guild_id = ?2
             ORDER BY discord_id",
        )?;
        statement
            .query_map(params![match_id, guild_id], |row| {
                Ok(MatchRatingHistoryEntry {
                    discord_id: row.get(0)?,
                    rating_before: row.get(1)?,
                    rating_after: row.get(2)?,
                    openskill_mu_before: row.get(3)?,
                    openskill_mu_after: row.get(4)?,
                    expected_team_win_prob: row.get(5)?,
                })
            })?
            .collect()
    }

    /// Get one player's outcomes, most recent match first.
    pub fn get_player_recent_outcomes(
        &self,
        discord_id: i64,
        guild_id: i64,
        limit: i64,
    ) -> Result<Vec<bool>, rusqlite::Error> {
        let connection = self.connection()?;
        query_outcomes(
            &connection,
            "SELECT won FROM rating_history
             WHERE discord_id = ?1 AND guild_id = ?2 AND won IS NOT NULL
             ORDER BY match_id DESC, id DESC
             LIMIT ?3",
            params![discord_id, guild_id, limit],
        )
    }

    /// Get capped histories for unique players while opening only one connection.
    pub fn get_player_recent_outcomes_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<HashMap<i64, Vec<bool>>, rusqlite::Error> {
        let unique_ids = unique_ids(discord_ids);
        let mut outcomes = unique_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, Vec::new()))
            .collect::<HashMap<_, _>>();
        if unique_ids.is_empty() {
            return Ok(outcomes);
        }

        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        for discord_id in unique_ids {
            let player_outcomes = query_outcomes(
                &connection,
                "SELECT won FROM rating_history
                 WHERE discord_id = ?1 AND guild_id = ?2 AND won IS NOT NULL
                 ORDER BY match_id DESC, id DESC
                 LIMIT ?3",
                params![discord_id, guild_id, limit],
            )?;
            outcomes.insert(discord_id, player_outcomes);
        }
        Ok(outcomes)
    }

    /// Get outcomes strictly before a target match for one player.
    ///
    /// A missing target history row returns an empty window even when earlier
    /// rows exist, matching the Python correction/replay safety guard.
    pub fn get_player_outcomes_before_match(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        match_id: i64,
        limit: i64,
    ) -> Result<Vec<bool>, rusqlite::Error> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        query_outcomes(
            &connection,
            "SELECT won FROM rating_history
             WHERE discord_id = ?1 AND guild_id = ?2 AND won IS NOT NULL
               AND (
                   match_id < ?3
                   OR (match_id IS NULL AND id < (
                       SELECT MIN(id) FROM rating_history
                       WHERE match_id = ?3 AND discord_id = ?1 AND guild_id = ?2
                   ))
               )
               AND EXISTS (
                   SELECT 1 FROM rating_history
                   WHERE match_id = ?3 AND discord_id = ?1 AND guild_id = ?2
               )
             ORDER BY match_id DESC, id DESC
             LIMIT ?4",
            params![discord_id, guild_id, match_id, limit],
        )
    }

    /// Get capped pre-match windows for unique players with one SQL query.
    pub fn get_player_outcomes_before_match_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        match_id: i64,
        limit: i64,
    ) -> Result<HashMap<i64, Vec<bool>>, rusqlite::Error> {
        let unique_ids = unique_ids(discord_ids);
        let mut outcomes = unique_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, Vec::new()))
            .collect::<HashMap<_, _>>();
        if unique_ids.is_empty() {
            return Ok(outcomes);
        }

        let placeholders = std::iter::repeat_n("?", unique_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "WITH target_rows AS (
                 SELECT discord_id, MIN(id) AS cutoff_id
                 FROM rating_history
                 WHERE match_id = ? AND guild_id = ?
                   AND discord_id IN ({placeholders})
                 GROUP BY discord_id
             ), ranked_outcomes AS (
                 SELECT
                     history.discord_id,
                     history.won,
                     ROW_NUMBER() OVER (
                         PARTITION BY history.discord_id
                         ORDER BY history.match_id DESC, history.id DESC
                     ) AS outcome_rank
                 FROM rating_history AS history
                 JOIN target_rows AS target
                   ON target.discord_id = history.discord_id
                 WHERE history.guild_id = ? AND history.won IS NOT NULL
                   AND (
                       history.match_id < ?
                       OR (
                           history.match_id IS NULL
                           AND history.id < target.cutoff_id
                       )
                   )
             )
             SELECT discord_id, won
             FROM ranked_outcomes
             WHERE outcome_rank <= ?
             ORDER BY discord_id, outcome_rank"
        );
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut parameters = Vec::with_capacity(unique_ids.len() + 5);
        parameters.push(Value::Integer(match_id));
        parameters.push(Value::Integer(guild_id));
        parameters.extend(unique_ids.iter().copied().map(Value::Integer));
        parameters.push(Value::Integer(guild_id));
        parameters.push(Value::Integer(match_id));
        parameters.push(Value::Integer(limit));

        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(parameters.iter()))?;
        while let Some(row) = rows.next()? {
            let discord_id = row.get(0)?;
            let won = python_truthy(row.get_ref(1)?);
            if let Some(player_outcomes) = outcomes.get_mut(&discord_id) {
                player_outcomes.push(won);
            }
        }
        Ok(outcomes)
    }
}

#[derive(Clone, Copy, Debug)]
struct RecalibrationPlayer {
    rating: Option<f64>,
    rd: Option<f64>,
    volatility: Option<f64>,
    os_sigma: Option<f64>,
    games_played: i64,
}

impl RecalibrationService {
    #[must_use]
    pub const fn new(
        repository: RatingHistoryRepository,
        config: RecalibrationServiceConfig,
    ) -> Self {
        Self { repository, config }
    }

    pub fn get_state_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<RecalibrationServiceState, RecalibrationRepositoryError> {
        let state = self
            .repository
            .get_recalibration_state(discord_id, guild_id)?;
        let last_recalibration_at = state.and_then(|state| state.last_recalibration_at);
        let cooldown_ends_at = last_recalibration_at
            .filter(|last| *last != 0)
            .map(|last| last.saturating_add(self.config.cooldown_seconds));
        let is_on_cooldown = cooldown_ends_at.is_some_and(|ends| now < ends);
        Ok(RecalibrationServiceState {
            discord_id,
            last_recalibration_at,
            total_recalibrations: state.map_or(0, |state| state.total_recalibrations),
            is_on_cooldown,
            cooldown_ends_at: is_on_cooldown.then_some(cooldown_ends_at).flatten(),
        })
    }

    pub fn can_recalibrate_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<RecalibrationEligibility, RecalibrationRepositoryError> {
        let Some(player) = self.repository.recalibration_player(discord_id, guild_id)? else {
            return Ok(RecalibrationEligibility::NotRegistered);
        };
        let (Some(current_rating), Some(current_rd), Some(current_volatility)) =
            (player.rating, player.rd, player.volatility)
        else {
            return Ok(RecalibrationEligibility::NoRating);
        };
        if player.games_played < self.config.min_games {
            return Ok(RecalibrationEligibility::InsufficientGames {
                games_played: player.games_played,
                min_games: self.config.min_games,
            });
        }
        let state = self.get_state_at(discord_id, guild_id, now)?;
        if let Some(cooldown_ends_at) = state.cooldown_ends_at {
            return Ok(RecalibrationEligibility::OnCooldown { cooldown_ends_at });
        }
        Ok(RecalibrationEligibility::Allowed {
            current_rating,
            current_rd,
            current_volatility,
            games_played: player.games_played,
            current_os_sigma: player.os_sigma,
        })
    }

    pub fn recalibrate_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<RecalibrationExecution, RecalibrationRepositoryError> {
        let eligibility = self.can_recalibrate_at(discord_id, guild_id, now)?;
        let RecalibrationEligibility::Allowed {
            current_rating,
            current_rd,
            current_volatility,
            current_os_sigma,
            ..
        } = eligibility
        else {
            return Ok(RecalibrationExecution::Rejected(eligibility));
        };
        let new_rd = current_rd.max(self.config.initial_rd);
        let new_os_sigma = current_os_sigma.map(|_| CamaOpenSkillSystem::DEFAULT_SIGMA);
        let total_recalibrations =
            match self
                .repository
                .execute_recalibration_atomic(RecalibrationRequest {
                    discord_id,
                    guild_id,
                    now,
                    cooldown_seconds: self.config.cooldown_seconds,
                    rating: current_rating,
                    new_rd,
                    new_volatility: self.config.initial_volatility,
                    new_os_sigma,
                }) {
                Ok(total) => total,
                Err(RecalibrationRepositoryError::OnCooldown { remaining_seconds }) => {
                    return Ok(RecalibrationExecution::Rejected(
                        RecalibrationEligibility::OnCooldown {
                            cooldown_ends_at: now.saturating_add(remaining_seconds),
                        },
                    ));
                }
                Err(error) => return Err(error),
            };
        Ok(RecalibrationExecution::Success(RecalibrationSuccess {
            old_rating: current_rating,
            old_rd: current_rd,
            old_volatility: current_volatility,
            new_rd,
            new_volatility: self.config.initial_volatility,
            old_os_sigma: current_os_sigma,
            new_os_sigma,
            total_recalibrations,
            cooldown_ends_at: now.saturating_add(self.config.cooldown_seconds),
        }))
    }

    pub fn reset_cooldown(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<RecalibrationCooldownReset, RecalibrationRepositoryError> {
        if self
            .repository
            .recalibration_player(discord_id, guild_id)?
            .is_none()
        {
            return Ok(RecalibrationCooldownReset::NotRegistered);
        }
        let state = self.get_state_at(discord_id, guild_id, now)?;
        if state.last_recalibration_at.is_none() {
            return Ok(RecalibrationCooldownReset::NoHistory);
        }
        self.repository
            .reset_recalibration_cooldown(discord_id, guild_id)?;
        Ok(RecalibrationCooldownReset::Reset {
            total_recalibrations: state.total_recalibrations,
        })
    }
}

fn increment_openskill_revision(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO openskill_rating_revisions (guild_id,revision,updated_at)
         VALUES (?1,1,CURRENT_TIMESTAMP)
         ON CONFLICT(guild_id) DO UPDATE SET
             revision=openskill_rating_revisions.revision+1,
             updated_at=CURRENT_TIMESTAMP",
        [guild_id],
    )?;
    Ok(())
}

fn query_outcomes<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<bool>, rusqlite::Error>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(parameters, |row| Ok(python_truthy(row.get_ref(0)?)))?
        .collect()
}

fn python_truthy(value: ValueRef<'_>) -> bool {
    match value {
        ValueRef::Null => false,
        ValueRef::Integer(value) => value != 0,
        ValueRef::Real(value) => value != 0.0,
        ValueRef::Text(value) | ValueRef::Blob(value) => !value.is_empty(),
    }
}

fn unique_ids(discord_ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(discord_ids.len());
    discord_ids
        .iter()
        .copied()
        .filter(|discord_id| seen.insert(*discord_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;

    use super::{
        NewRatingHistoryEntry, RatingHistoryRepository, RecalibrationCooldownReset,
        RecalibrationEligibility, RecalibrationExecution, RecalibrationRepositoryError,
        RecalibrationRequest, RecalibrationService, RecalibrationServiceConfig,
        RecalibrationStateUpsert,
    };
    use crate::test_support::FastTestDatabase;
    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    const TEST_GUILD_ID: i64 = 987_654_321;

    enum FixtureDatabase {
        Fast(FastTestDatabase),
        Durable(NamedTempFile),
    }

    impl FixtureDatabase {
        fn path(&self) -> &Path {
            match self {
                Self::Fast(database) => database.path(),
                Self::Durable(database) => database.path(),
            }
        }
    }

    struct Fixture {
        _file: FixtureDatabase,
        repository: RatingHistoryRepository,
    }

    struct RecalibrationPlayerSeed {
        discord_id: i64,
        guild_id: i64,
        rating: Option<f64>,
        rd: Option<f64>,
        volatility: Option<f64>,
        os_sigma: Option<f64>,
        games_played: i64,
    }

    fn fixture_database(durable: bool) -> FixtureDatabase {
        static TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();
        let template = TEMPLATE.get_or_init(|| {
            let file = NamedTempFile::new().expect("create temporary SQLite file");
            let connection = Connection::open(file.path()).expect("open temporary SQLite file");
            connection
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE players (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL DEFAULT 0,
                         discord_username TEXT NOT NULL,
                         glicko_rating REAL,
                         glicko_rd REAL,
                         glicko_volatility REAL,
                         os_sigma REAL,
                         wins INTEGER NOT NULL DEFAULT 0,
                         losses INTEGER NOT NULL DEFAULT 0,
                         os_rating_version INTEGER,
                         os_algorithm_fingerprint TEXT,
                         updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                         PRIMARY KEY (discord_id, guild_id)
                     );
                     CREATE TABLE recalibration_state (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL DEFAULT 0,
                         last_recalibration_at INTEGER,
                         total_recalibrations INTEGER DEFAULT 0,
                         rating_at_recalibration REAL,
                         updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                         PRIMARY KEY (discord_id, guild_id)
                     );
                     CREATE TABLE openskill_rating_events (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL,
                         discord_id INTEGER NOT NULL,
                         event_type TEXT NOT NULL,
                         value REAL NOT NULL,
                         event_at TEXT NOT NULL,
                         reason TEXT,
                         actor_id INTEGER,
                         os_algorithm_version INTEGER NOT NULL,
                         os_algorithm_fingerprint TEXT NOT NULL
                     );
                     CREATE TABLE openskill_rating_revisions (
                         guild_id INTEGER PRIMARY KEY,
                         revision INTEGER NOT NULL DEFAULT 0,
                         updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                     );
                     CREATE TABLE matches (
                         match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL DEFAULT 0,
                         team1_players TEXT NOT NULL DEFAULT '[]',
                         team2_players TEXT NOT NULL DEFAULT '[]',
                         winning_team INTEGER,
                         match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                     );
                     CREATE TABLE rating_history (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         discord_id INTEGER,
                         guild_id INTEGER NOT NULL DEFAULT 0,
                         rating REAL,
                         rating_before REAL,
                         rd_before REAL,
                         rd_after REAL,
                         volatility_before REAL,
                         volatility_after REAL,
                         expected_team_win_prob REAL,
                         team_number INTEGER,
                         won BOOLEAN,
                         match_id INTEGER,
                         timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                         os_mu_before REAL,
                         os_mu_after REAL,
                         os_sigma_before REAL,
                         os_sigma_after REAL,
                         fantasy_weight REAL,
                         os_algorithm_version INTEGER,
                         os_algorithm_fingerprint TEXT,
                         streak_length INTEGER,
                         streak_multiplier REAL,
                         streak_multiplier_per_game REAL NOT NULL DEFAULT 0.20,
                         base_rating_delta_multiplier REAL NOT NULL DEFAULT 0.75,
                         low_priority_gain_multiplier REAL NOT NULL DEFAULT 1.0,
                         streak_threshold INTEGER NOT NULL DEFAULT 3
                     );
                     CREATE INDEX idx_rating_history_guild_player_time
                         ON rating_history(guild_id, discord_id, timestamp DESC);
                     CREATE INDEX idx_rating_history_guild_time
                         ON rating_history(guild_id, timestamp DESC);",
                )
                .expect("create Python-compatible rating history schema");
            drop(connection);
            file
        });
        if durable {
            let file = NamedTempFile::new().expect("durable rating-history database");
            std::fs::copy(template.path(), file.path())
                .expect("copy rating-history fixture template");
            FixtureDatabase::Durable(file)
        } else {
            FixtureDatabase::Fast(FastTestDatabase::from_template(template.path()))
        }
    }

    impl Fixture {
        fn new() -> Self {
            Self::from_database(fixture_database(false))
        }

        fn durable() -> Self {
            Self::from_database(fixture_database(true))
        }

        fn from_database(file: FixtureDatabase) -> Self {
            let repository = RatingHistoryRepository::new(file.path());
            Self {
                _file: file,
                repository,
            }
        }

        fn record_match(&self) -> i64 {
            let connection = Connection::open(self._file.path()).expect("open match fixture DB");
            connection
                .execute(
                    "INSERT INTO matches (guild_id) VALUES (?1)",
                    [TEST_GUILD_ID],
                )
                .expect("insert chronological match");
            connection.last_insert_rowid()
        }

        fn add_outcome(&self, discord_id: i64, match_id: i64, won: bool) -> i64 {
            let mut entry = NewRatingHistoryEntry::new(discord_id, TEST_GUILD_ID, 1500.0);
            entry.match_id = Some(match_id);
            entry.won = Some(won);
            self.repository
                .add_rating_history(&entry)
                .expect("insert rating history outcome")
        }

        fn seed_recalibration_player(
            &self,
            discord_id: i64,
            rating: Option<f64>,
            rd: Option<f64>,
            volatility: Option<f64>,
            os_sigma: Option<f64>,
            games_played: i64,
        ) {
            self.seed_recalibration_player_in_guild(RecalibrationPlayerSeed {
                discord_id,
                guild_id: TEST_GUILD_ID,
                rating,
                rd,
                volatility,
                os_sigma,
                games_played,
            });
        }

        fn seed_recalibration_player_in_guild(&self, seed: RecalibrationPlayerSeed) {
            Connection::open(self._file.path())
                .expect("open recalibration fixture")
                .execute(
                    "INSERT INTO players (
                         discord_id,guild_id,discord_username,glicko_rating,
                         glicko_rd,glicko_volatility,os_sigma,wins,losses
                     ) VALUES (?1,?2,'TestPlayer',?3,?4,?5,?6,?7,0)",
                    params![
                        seed.discord_id,
                        seed.guild_id,
                        seed.rating,
                        seed.rd,
                        seed.volatility,
                        seed.os_sigma,
                        seed.games_played,
                    ],
                )
                .expect("seed recalibration player");
        }
    }

    #[test]
    fn test_recent_rating_history_queries_use_chronology_indexes() {
        let fixture = Fixture::new();
        let connection = Connection::open(fixture._file.path()).expect("open history fixture");

        let guild_plan = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT * FROM rating_history
                 WHERE guild_id = ?1
                 ORDER BY timestamp DESC
                 LIMIT ?2",
            )
            .expect("prepare guild history query plan")
            .query_map(params![TEST_GUILD_ID, 2_000_i64], |row| {
                row.get::<_, String>(3)
            })
            .expect("explain guild history query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect guild history query plan");
        let player_plan = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT * FROM rating_history
                 WHERE discord_id = ?1 AND guild_id = ?2
                 ORDER BY timestamp DESC
                 LIMIT ?3",
            )
            .expect("prepare player history query plan")
            .query_map(params![1_i64, TEST_GUILD_ID, 50_i64], |row| {
                row.get::<_, String>(3)
            })
            .expect("explain player history query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect player history query plan");

        assert!(
            guild_plan
                .iter()
                .any(|detail| detail.contains("idx_rating_history_guild_time"))
        );
        assert!(
            player_plan
                .iter()
                .any(|detail| detail.contains("idx_rating_history_guild_player_time"))
        );
        assert!(
            !guild_plan
                .iter()
                .any(|detail| detail.contains("TEMP B-TREE FOR ORDER BY"))
        );
        assert!(
            !player_plan
                .iter()
                .any(|detail| detail.contains("TEMP B-TREE FOR ORDER BY"))
        );
    }

    fn recalibration_service(fixture: &Fixture) -> RecalibrationService {
        RecalibrationService::new(
            fixture.repository.clone(),
            RecalibrationServiceConfig {
                cooldown_seconds: 3_600,
                initial_rd: 350.0,
                initial_volatility: 0.06,
                min_games: 5,
            },
        )
    }

    #[test]
    fn test_get_state_not_exists() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .repository
                .get_recalibration_state(99_999, Some(TEST_GUILD_ID))
                .expect("read absent recalibration state"),
            None
        );
    }

    #[test]
    fn test_upsert_state_create() {
        let fixture = Fixture::new();
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: Some(TEST_GUILD_ID),
                last_recalibration_at: Some(1_234_567),
                total_recalibrations: Some(1),
                rating_at_recalibration: Some(1_500.0),
            })
            .expect("create recalibration state");

        let state = fixture
            .repository
            .get_recalibration_state(12_345, Some(TEST_GUILD_ID))
            .expect("read recalibration state")
            .expect("created recalibration state");
        assert_eq!(state.discord_id, 12_345);
        assert_eq!(state.last_recalibration_at, Some(1_234_567));
        assert_eq!(state.total_recalibrations, 1);
        assert_eq!(state.rating_at_recalibration, Some(1_500.0));
    }

    #[test]
    fn test_upsert_state_update() {
        let fixture = Fixture::new();
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: Some(TEST_GUILD_ID),
                last_recalibration_at: Some(1_000),
                total_recalibrations: Some(1),
                rating_at_recalibration: Some(1_500.0),
            })
            .expect("create recalibration state");
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: Some(TEST_GUILD_ID),
                last_recalibration_at: Some(2_000),
                total_recalibrations: Some(2),
                rating_at_recalibration: None,
            })
            .expect("update recalibration state");

        let state = fixture
            .repository
            .get_recalibration_state(12_345, Some(TEST_GUILD_ID))
            .expect("read recalibration state")
            .expect("updated recalibration state");
        assert_eq!(state.last_recalibration_at, Some(2_000));
        assert_eq!(state.total_recalibrations, 2);
        assert_eq!(state.rating_at_recalibration, Some(1_500.0));
    }

    #[test]
    fn test_reset_cooldown() {
        let fixture = Fixture::new();
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: Some(TEST_GUILD_ID),
                last_recalibration_at: Some(2_000),
                total_recalibrations: Some(1),
                rating_at_recalibration: None,
            })
            .expect("create recalibration state");

        assert!(
            fixture
                .repository
                .reset_recalibration_cooldown(12_345, Some(TEST_GUILD_ID))
                .expect("reset cooldown")
        );
        let state = fixture
            .repository
            .get_recalibration_state(12_345, Some(TEST_GUILD_ID))
            .expect("read recalibration state")
            .expect("reset recalibration state");
        assert_eq!(state.last_recalibration_at, Some(0));
        assert_eq!(state.total_recalibrations, 1);
    }

    #[test]
    fn test_get_state_no_history() {
        let fixture = Fixture::new();
        let state = recalibration_service(&fixture)
            .get_state_at(12_345, Some(TEST_GUILD_ID), 10_000)
            .expect("read default service state");
        assert_eq!(state.discord_id, 12_345);
        assert_eq!(state.last_recalibration_at, None);
        assert_eq!(state.total_recalibrations, 0);
        assert!(!state.is_on_cooldown);
        assert_eq!(state.cooldown_ends_at, None);
    }

    #[test]
    fn test_can_recalibrate_not_registered() {
        let fixture = Fixture::new();
        assert_eq!(
            recalibration_service(&fixture)
                .can_recalibrate_at(99_999, Some(TEST_GUILD_ID), 10_000)
                .expect("check absent player"),
            RecalibrationEligibility::NotRegistered
        );
    }

    #[test]
    fn test_can_recalibrate_insufficient_games() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(12_345, Some(1_500.0), Some(200.0), Some(0.06), None, 0);
        assert_eq!(
            recalibration_service(&fixture)
                .can_recalibrate_at(12_345, Some(TEST_GUILD_ID), 10_000)
                .expect("check new player"),
            RecalibrationEligibility::InsufficientGames {
                games_played: 0,
                min_games: 5,
            }
        );
    }

    #[test]
    fn test_can_recalibrate_success() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(12_345, Some(1_500.0), Some(200.0), Some(0.06), None, 5);
        assert_eq!(
            recalibration_service(&fixture)
                .can_recalibrate_at(12_345, Some(TEST_GUILD_ID), 10_000)
                .expect("check eligible player"),
            RecalibrationEligibility::Allowed {
                current_rating: 1_500.0,
                current_rd: 200.0,
                current_volatility: 0.06,
                games_played: 5,
                current_os_sigma: None,
            }
        );
    }

    #[test]
    fn test_can_recalibrate_on_cooldown() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(12_345, Some(1_500.0), Some(80.0), Some(0.06), None, 5);
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: Some(TEST_GUILD_ID),
                last_recalibration_at: Some(10_000),
                total_recalibrations: Some(1),
                rating_at_recalibration: None,
            })
            .expect("seed active cooldown");
        assert_eq!(
            recalibration_service(&fixture)
                .can_recalibrate_at(12_345, Some(TEST_GUILD_ID), 10_000)
                .expect("check cooldown"),
            RecalibrationEligibility::OnCooldown {
                cooldown_ends_at: 13_600,
            }
        );
    }

    #[test]
    fn test_recalibrate_success() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(
            12_345,
            Some(1_500.0),
            Some(80.0),
            Some(0.05),
            Some(3.0),
            5,
        );
        let result = recalibration_service(&fixture)
            .recalibrate_at(12_345, Some(TEST_GUILD_ID), 10_000)
            .expect("recalibrate player");
        let RecalibrationExecution::Success(success) = result else {
            panic!("eligible player should recalibrate: {result:?}");
        };
        assert_eq!(success.old_rating, 1_500.0);
        assert_eq!(success.old_rd, 80.0);
        assert_eq!(success.new_rd, 350.0);
        assert_eq!(success.new_volatility, 0.06);
        assert_eq!(success.total_recalibrations, 1);
        assert_eq!(success.cooldown_ends_at, 13_600);

        let (rating, rd, volatility, sigma): (f64, f64, f64, f64) =
            Connection::open(fixture._file.path())
                .expect("open recalibrated player")
                .query_row(
                    "SELECT glicko_rating,glicko_rd,glicko_volatility,os_sigma
                     FROM players WHERE discord_id=12345 AND guild_id=?1",
                    [TEST_GUILD_ID],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .expect("read recalibrated player");
        assert_eq!((rating, rd, volatility), (1_500.0, 350.0, 0.06));
        assert!((sigma - (25.0 / 3.0)).abs() < 1e-12);
    }

    #[test]
    fn test_recalibrate_preserves_rating() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(12_345, Some(2_000.0), Some(50.0), Some(0.04), None, 5);
        assert!(matches!(
            recalibration_service(&fixture)
                .recalibrate_at(12_345, Some(TEST_GUILD_ID), 10_000)
                .expect("recalibrate player"),
            RecalibrationExecution::Success(_)
        ));
        let rating: f64 = Connection::open(fixture._file.path())
            .expect("open recalibrated player")
            .query_row(
                "SELECT glicko_rating FROM players
                 WHERE discord_id=12345 AND guild_id=?1",
                [TEST_GUILD_ID],
                |row| row.get(0),
            )
            .expect("read preserved rating");
        assert_eq!(rating, 2_000.0);
    }

    #[test]
    fn test_recalibrate_increments_count() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(12_345, Some(1_500.0), Some(80.0), Some(0.06), None, 5);
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: Some(TEST_GUILD_ID),
                last_recalibration_at: Some(2_800),
                total_recalibrations: Some(2),
                rating_at_recalibration: None,
            })
            .expect("seed expired cooldown");
        let result = recalibration_service(&fixture)
            .recalibrate_at(12_345, Some(TEST_GUILD_ID), 10_000)
            .expect("recalibrate player");
        let RecalibrationExecution::Success(success) = result else {
            panic!("eligible player should recalibrate: {result:?}");
        };
        assert_eq!(success.total_recalibrations, 3);
        assert_eq!(
            recalibration_service(&fixture)
                .get_state_at(12_345, Some(TEST_GUILD_ID), 10_000)
                .expect("read updated state")
                .total_recalibrations,
            3
        );
    }

    #[test]
    fn test_service_reset_cooldown() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(12_345, Some(1_500.0), Some(80.0), Some(0.06), None, 0);
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: Some(TEST_GUILD_ID),
                last_recalibration_at: Some(10_000),
                total_recalibrations: Some(1),
                rating_at_recalibration: None,
            })
            .expect("seed active cooldown");
        assert_eq!(
            recalibration_service(&fixture)
                .reset_cooldown(12_345, Some(TEST_GUILD_ID), 10_000)
                .expect("reset service cooldown"),
            RecalibrationCooldownReset::Reset {
                total_recalibrations: 1,
            }
        );
        let state = recalibration_service(&fixture)
            .get_state_at(12_345, Some(TEST_GUILD_ID), 10_000)
            .expect("read reset state");
        assert!(!state.is_on_cooldown);
        assert_eq!(state.total_recalibrations, 1);
    }

    #[test]
    fn test_reset_cooldown_no_history() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player(12_345, Some(1_500.0), Some(80.0), Some(0.06), None, 0);
        assert_eq!(
            recalibration_service(&fixture)
                .reset_cooldown(12_345, Some(TEST_GUILD_ID), 10_000)
                .expect("reject missing history"),
            RecalibrationCooldownReset::NoHistory
        );
    }

    #[test]
    fn test_fix6_can_recalibrate_and_recalibrate_agree_on_none_guild() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player_in_guild(RecalibrationPlayerSeed {
            discord_id: 12_345,
            guild_id: 0,
            rating: Some(1_500.0),
            rd: Some(80.0),
            volatility: Some(0.05),
            os_sigma: Some(3.0),
            games_played: 5,
        });
        let service = recalibration_service(&fixture);
        assert!(matches!(
            service
                .can_recalibrate_at(12_345, None, 10_000)
                .expect("normalized eligibility"),
            RecalibrationEligibility::Allowed { .. }
        ));
        assert!(matches!(
            service
                .recalibrate_at(12_345, None, 10_000)
                .expect("normalized recalibration"),
            RecalibrationExecution::Success(_)
        ));
        assert_eq!(
            fixture
                .repository
                .get_recalibration_state(12_345, None)
                .expect("normalized state")
                .expect("persisted normalized state")
                .guild_id,
            0
        );
    }

    #[test]
    fn test_fix6_reset_cooldown_normalizes_none_guild_to_zero() {
        let fixture = Fixture::new();
        fixture.seed_recalibration_player_in_guild(RecalibrationPlayerSeed {
            discord_id: 12_345,
            guild_id: 0,
            rating: Some(1_500.0),
            rd: Some(80.0),
            volatility: Some(0.06),
            os_sigma: None,
            games_played: 0,
        });
        fixture
            .repository
            .upsert_recalibration_state(RecalibrationStateUpsert {
                discord_id: 12_345,
                guild_id: None,
                last_recalibration_at: Some(10_000),
                total_recalibrations: Some(2),
                rating_at_recalibration: Some(1_500.0),
            })
            .expect("seed normalized cooldown");
        assert_eq!(
            recalibration_service(&fixture)
                .reset_cooldown(12_345, None, 10_000)
                .expect("reset normalized cooldown"),
            RecalibrationCooldownReset::Reset {
                total_recalibrations: 2,
            }
        );
        assert_eq!(
            fixture
                .repository
                .get_recalibration_state(12_345, Some(0))
                .expect("read normalized cooldown")
                .expect("normalized cooldown row")
                .last_recalibration_at,
            Some(0)
        );
    }

    mod match_repository_recent_outcomes {
        use super::*;

        #[test]
        fn test_get_player_recent_outcomes_returns_booleans() {
            let fixture = Fixture::new();
            let discord_id = 12_345;
            for won in [true, false, true] {
                let match_id = fixture.record_match();
                fixture.add_outcome(discord_id, match_id, won);
            }

            let outcomes = fixture
                .repository
                .get_player_recent_outcomes(discord_id, TEST_GUILD_ID, 10)
                .unwrap();
            assert_eq!(outcomes, [true, false, true]);
        }

        #[test]
        fn test_get_player_recent_outcomes_respects_limit() {
            let fixture = Fixture::new();
            let discord_id = 12_345;
            for _ in 0..10 {
                let match_id = fixture.record_match();
                fixture.add_outcome(discord_id, match_id, true);
            }

            let outcomes = fixture
                .repository
                .get_player_recent_outcomes(discord_id, TEST_GUILD_ID, 5)
                .unwrap();
            assert_eq!(outcomes.len(), 5);
        }

        #[test]
        fn test_get_player_recent_outcomes_empty_for_new_player() {
            let fixture = Fixture::new();
            let outcomes = fixture
                .repository
                .get_player_recent_outcomes(12_345, TEST_GUILD_ID, 10)
                .unwrap();
            assert!(outcomes.is_empty());
        }

        #[test]
        fn test_get_player_recent_outcomes_returns_most_recent_first() {
            let fixture = Fixture::new();
            let discord_id = 12_345;
            let chronological = [true, true, false, false, true];
            for won in chronological {
                let match_id = fixture.record_match();
                fixture.add_outcome(discord_id, match_id, won);
            }

            let outcomes = fixture
                .repository
                .get_player_recent_outcomes(discord_id, TEST_GUILD_ID, 10)
                .unwrap();
            assert_eq!(outcomes, [true, false, false, true, true]);
        }
    }

    mod rating_history_streak_columns {
        use super::*;

        #[test]
        fn test_add_rating_history_with_streak_data() {
            let fixture = Fixture::new();
            let match_id = fixture.record_match();
            let mut entry = NewRatingHistoryEntry::new(12_345, TEST_GUILD_ID, 1520.0);
            entry.match_id = Some(match_id);
            entry.rating_before = Some(1500.0);
            entry.won = Some(true);
            entry.streak_length = Some(5);
            entry.streak_multiplier = Some(1.30);
            fixture.repository.add_rating_history(&entry).unwrap();

            let history = fixture
                .repository
                .get_rating_history(12_345, TEST_GUILD_ID, 1)
                .unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].streak_length, Some(5));
            assert_eq!(history[0].streak_multiplier, Some(1.30));
        }

        #[test]
        fn test_streak_columns_default_to_none() {
            let fixture = Fixture::new();
            let match_id = fixture.record_match();
            let mut entry = NewRatingHistoryEntry::new(12_345, TEST_GUILD_ID, 1520.0);
            entry.match_id = Some(match_id);
            entry.won = Some(true);
            fixture.repository.add_rating_history(&entry).unwrap();

            let history = fixture
                .repository
                .get_rating_history(12_345, TEST_GUILD_ID, 1)
                .unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].streak_length, None);
            assert_eq!(history[0].streak_multiplier, None);
        }
    }

    mod chronological_outcome_windows {
        use super::*;

        fn setup_history(fixture: &Fixture) -> (i64, [i64; 3]) {
            let discord_id = 31_337;
            let match_ids = [
                fixture.record_match(),
                fixture.record_match(),
                fixture.record_match(),
            ];
            let [m1, m2, m3] = match_ids;
            // Backfill the oldest match last, so its history id is newest.
            fixture.add_outcome(discord_id, m2, false);
            fixture.add_outcome(discord_id, m3, false);
            fixture.add_outcome(discord_id, m1, true);
            (discord_id, [m1, m2, m3])
        }

        #[test]
        fn test_recent_outcomes_follow_match_chronology() {
            let fixture = Fixture::new();
            let (discord_id, _) = setup_history(&fixture);
            let outcomes = fixture
                .repository
                .get_player_recent_outcomes(discord_id, TEST_GUILD_ID, 10)
                .unwrap();
            assert_eq!(outcomes, [false, false, true]);
        }

        #[test]
        fn test_recent_outcomes_bulk_follow_match_chronology() {
            let fixture = Fixture::new();
            let (discord_id, _) = setup_history(&fixture);
            let outcomes = fixture
                .repository
                .get_player_recent_outcomes_bulk(&[discord_id], Some(TEST_GUILD_ID), 10)
                .unwrap();
            assert_eq!(outcomes[&discord_id], [false, false, true]);
        }

        #[test]
        fn test_outcomes_before_match_follow_match_chronology() {
            let fixture = Fixture::new();
            let (discord_id, [m1, _, m3]) = setup_history(&fixture);
            let outcomes = fixture
                .repository
                .get_player_outcomes_before_match(discord_id, Some(TEST_GUILD_ID), m3, 10)
                .unwrap();
            assert_eq!(outcomes, [false, true]);

            let oldest = fixture
                .repository
                .get_player_outcomes_before_match(discord_id, Some(TEST_GUILD_ID), m1, 10)
                .unwrap();
            assert!(oldest.is_empty());
        }

        #[test]
        fn test_outcomes_before_match_bulk_follow_match_chronology() {
            let fixture = Fixture::new();
            let (discord_id, [_, _, m3]) = setup_history(&fixture);
            let outcomes = fixture
                .repository
                .get_player_outcomes_before_match_bulk(&[discord_id], Some(TEST_GUILD_ID), m3, 10)
                .unwrap();
            assert_eq!(outcomes[&discord_id], [false, true]);
        }

        #[test]
        fn test_outcomes_before_match_empty_without_history_row() {
            let fixture = Fixture::new();
            let (discord_id, _) = setup_history(&fixture);
            let extra_match = fixture.record_match();
            let outcomes = fixture
                .repository
                .get_player_outcomes_before_match(discord_id, Some(TEST_GUILD_ID), extra_match, 10)
                .unwrap();
            assert!(outcomes.is_empty());

            let bulk = fixture
                .repository
                .get_player_outcomes_before_match_bulk(
                    &[discord_id],
                    Some(TEST_GUILD_ID),
                    extra_match,
                    10,
                )
                .unwrap();
            assert!(bulk[&discord_id].is_empty());
        }
    }

    #[test]
    fn python_truthiness_and_null_filter_interoperate() {
        let fixture = Fixture::new();
        let discord_id = 77;
        for (match_id, raw_won) in [(1, None), (2, Some(0)), (3, Some(-1))] {
            let connection = Connection::open(fixture._file.path()).unwrap();
            connection
                .execute(
                    "INSERT INTO rating_history
                     (discord_id, guild_id, rating, match_id, won)
                     VALUES (?1, ?2, 1500, ?3, ?4)",
                    params![discord_id, TEST_GUILD_ID, match_id, raw_won],
                )
                .unwrap();
        }
        let outcomes = fixture
            .repository
            .get_player_recent_outcomes(discord_id, TEST_GUILD_ID, 10)
            .unwrap();
        assert_eq!(outcomes, [true, false]);
    }

    #[test]
    fn test_concurrent_recalibrations_bump_counter_once() {
        let fixture = Fixture::durable();
        Connection::open(fixture._file.path())
            .expect("open fixture")
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username,
                     glicko_rating, glicko_rd, glicko_volatility
                 ) VALUES (1, ?1, 'u1', 1500.0, 200.0, 0.06)",
                [TEST_GUILD_ID],
            )
            .expect("seed recalibration player");
        let repository = Arc::new(fixture.repository.clone());
        let barrier = Arc::new(Barrier::new(5));
        let handles = (0..5)
            .map(|_| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository.execute_recalibration_atomic(RecalibrationRequest {
                        discord_id: 1,
                        guild_id: Some(TEST_GUILD_ID),
                        now: 1_000_000,
                        cooldown_seconds: 3_600,
                        rating: 1_500.0,
                        new_rd: 300.0,
                        new_volatility: 0.06,
                        new_os_sigma: Some(7.5),
                    })
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("recalibration thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(
            results
                .iter()
                .filter_map(|result| result.as_ref().err())
                .all(
                    |error| matches!(error, RecalibrationRepositoryError::OnCooldown { .. })
                        && error.to_string().starts_with("ON_COOLDOWN")
                )
        );
        let state = repository
            .get_recalibration_state(1, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!(state.total_recalibrations, 1);
        let rd: f64 = Connection::open(fixture._file.path())
            .expect("open fixture")
            .query_row(
                "SELECT glicko_rd FROM players
                 WHERE discord_id = 1 AND guild_id = ?1",
                [TEST_GUILD_ID],
                |row| row.get(0),
            )
            .expect("Glicko RD");
        assert_eq!(rd, 300.0);
        let connection = Connection::open(fixture._file.path()).expect("open fixture");
        let sigma: f64 = connection
            .query_row(
                "SELECT os_sigma FROM players
                 WHERE discord_id = 1 AND guild_id = ?1",
                [TEST_GUILD_ID],
                |row| row.get(0),
            )
            .expect("OpenSkill sigma");
        assert_eq!(sigma, 7.5);
        let (event_type, event_value, reason, actor_id): (String, f64, String, i64) = connection
            .query_row(
                "SELECT event_type,value,reason,actor_id
                   FROM openskill_rating_events
                  WHERE guild_id=?1 AND discord_id=1",
                [TEST_GUILD_ID],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("durable OpenSkill event");
        assert_eq!(event_type, "set_sigma");
        assert_eq!(event_value, 7.5);
        assert_eq!(reason, "player_recalibration");
        assert_eq!(actor_id, 1);
        let revision: i64 = connection
            .query_row(
                "SELECT revision FROM openskill_rating_revisions WHERE guild_id=?1",
                [TEST_GUILD_ID],
                |row| row.get(0),
            )
            .expect("OpenSkill revision");
        assert_eq!(revision, 1);
    }

    #[test]
    fn connection_matches_python_runtime_settings() {
        let fixture = Fixture::new();
        let connection = fixture.repository.connection().unwrap();
        let timeout: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5_000);
        assert_eq!(foreign_keys, 0);
    }

    #[test]
    fn outcome_window_feeds_domain_streak_policy_without_conversion() {
        use cama_domain::rating::CamaRatingSystem;

        let fixture = Fixture::new();
        let discord_id = 404;
        for _ in 0..3 {
            let match_id = fixture.record_match();
            fixture.add_outcome(discord_id, match_id, true);
        }
        let outcomes = fixture
            .repository
            .get_player_recent_outcomes(discord_id, TEST_GUILD_ID, 20)
            .unwrap();
        let adjustment =
            CamaRatingSystem::default().calculate_streak_multiplier(&outcomes, true, None, None);
        assert_eq!(adjustment.streak_length, 4);
        assert!((adjustment.multiplier - 1.60).abs() < f64::EPSILON);
    }
}
