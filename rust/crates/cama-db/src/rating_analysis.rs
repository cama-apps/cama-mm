//! Existing-schema SQLite adapter for the production `/ratinganalysis` command.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cama_domain::openskill::CamaOpenSkillSystem;
use rusqlite::{OptionalExtension, params};
use thiserror::Error;

use crate::core_repositories::{MatchRepository, PlayerRepository};
use crate::match_correction_repository::MatchCorrectionRepository;
use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RatingAnalysisBackfill {
    pub matches_processed: usize,
    pub total_matches: usize,
    pub players_updated: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RatingAnalysisPlayer {
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
    pub os_mu: Option<f64>,
    pub os_sigma: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RatingAnalysisHistoryRow {
    pub os_mu_before: f64,
    pub os_mu_after: f64,
    pub os_sigma_before: f64,
    pub os_sigma_after: f64,
    pub won: Option<bool>,
    pub fantasy_weight: Option<f64>,
    pub streak_length: Option<i64>,
    pub streak_multiplier: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatingAnalysisHistoricalMatch {
    pub match_id: i64,
    pub match_date: String,
    pub winning_team: i32,
    pub expected_radiant_win_prob: Option<f64>,
    pub team1_players: Vec<i64>,
    pub team2_players: Vec<i64>,
    pub openskill_radiant_win_prob: Option<f64>,
    pub openskill_raw_radiant_win_prob: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatingAnalysisHistoricalRatings {
    pub team1: Vec<(f64, f64)>,
    pub team2: Vec<(f64, f64)>,
}

#[derive(Debug, Error)]
pub enum RatingAnalysisRepositoryError {
    #[error("SQLite rating-analysis operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("rating-analysis JSON data is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OpenSkill replay failed: {0}")]
    Replay(String),
}

/// Bounded adapter over the migrated production schema. It never creates or
/// upgrades tables; startup admission remains the sole migration authority.
#[derive(Clone, Debug)]
pub struct RatingAnalysisRepository {
    path: PathBuf,
    openskill: CamaOpenSkillSystem,
    new_player_mmr_discount: i32,
}

impl RatingAnalysisRepository {
    #[must_use]
    pub fn new(
        path: impl AsRef<Path>,
        openskill: CamaOpenSkillSystem,
        new_player_mmr_discount: i32,
    ) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            openskill,
            new_player_mmr_discount,
        }
    }

    pub fn backfill_openskill(
        &self,
        guild_id: Option<i64>,
    ) -> Result<RatingAnalysisBackfill, RatingAnalysisRepositoryError> {
        let correction = MatchCorrectionRepository::new(&self.path);
        correction
            .mark_openskill_replay_pending(guild_id, "ratinganalysis:backfill")
            .map_err(|error| RatingAnalysisRepositoryError::Replay(error.to_string()))?;
        let (summary, total_matches) = correction
            .replay_openskill_atomic_configured(
                guild_id,
                &self.openskill,
                self.new_player_mmr_discount,
            )
            .map_err(|error| RatingAnalysisRepositoryError::Replay(error.to_string()))?;
        Ok(RatingAnalysisBackfill {
            matches_processed: summary.matches_processed,
            total_matches,
            players_updated: summary.players_updated,
            errors: summary.errors,
        })
    }

    pub fn player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RatingAnalysisPlayer>, RatingAnalysisRepositoryError> {
        PlayerRepository::new(&self.path)
            .get_by_id(discord_id, guild_id)
            .map(|player| {
                player.map(|player| RatingAnalysisPlayer {
                    glicko_rating: player.glicko_rating,
                    glicko_rd: player.glicko_rd,
                    os_mu: player.os_mu,
                    os_sigma: player.os_sigma,
                })
            })
            .map_err(|error| RatingAnalysisRepositoryError::Replay(error.to_string()))
    }

    pub fn player_openskill_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<RatingAnalysisHistoryRow>, RatingAnalysisRepositoryError> {
        let guild_id = PlayerRepository::normalize_guild_id(guild_id);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT rh.os_mu_before,rh.os_mu_after,rh.os_sigma_before,
                    rh.os_sigma_after,rh.fantasy_weight,rh.streak_length,
                    rh.streak_multiplier,rh.won
             FROM rating_history rh
             JOIN matches m ON rh.match_id=m.match_id
             WHERE rh.discord_id=?1 AND rh.guild_id=?2
               AND rh.os_mu_before IS NOT NULL AND rh.os_mu_after IS NOT NULL
             ORDER BY m.match_date DESC LIMIT ?3",
        )?;
        statement
            .query_map(params![discord_id, guild_id, limit], |row| {
                Ok(RatingAnalysisHistoryRow {
                    os_mu_before: row.get(0)?,
                    os_mu_after: row.get(1)?,
                    os_sigma_before: row.get(2)?,
                    os_sigma_after: row.get(3)?,
                    fantasy_weight: row.get(4)?,
                    streak_length: row.get(5)?,
                    streak_multiplier: row.get(6)?,
                    won: row.get::<_, Option<i64>>(7)?.map(|value| value != 0),
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn all_matches_with_predictions(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<RatingAnalysisHistoricalMatch>, RatingAnalysisRepositoryError> {
        let guild_id = PlayerRepository::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT m.match_id,m.winning_team,m.match_date,m.team1_players,
                    m.team2_players,mp.expected_radiant_win_prob,
                    mp.openskill_radiant_win_prob,
                    mp.openskill_raw_radiant_win_prob
             FROM matches m JOIN match_predictions mp ON m.match_id=mp.match_id
             WHERE m.guild_id=?1 ORDER BY m.match_date ASC",
        )?;
        let rows = statement
            .query_map([guild_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, rusqlite::types::Value>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<f64>>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| {
                Ok(RatingAnalysisHistoricalMatch {
                    match_id: row.0,
                    winning_team: i32::try_from(row.1).unwrap_or_default(),
                    match_date: sqlite_text(row.2),
                    team1_players: serde_json::from_str(&row.3)?,
                    team2_players: serde_json::from_str(&row.4)?,
                    expected_radiant_win_prob: row.5,
                    openskill_radiant_win_prob: row.6,
                    openskill_raw_radiant_win_prob: row.7,
                })
            })
            .collect()
    }

    pub fn openskill_ratings_for_matches(
        &self,
        match_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashMap<i64, RatingAnalysisHistoricalRatings>, RatingAnalysisRepositoryError> {
        MatchRepository::new(&self.path)
            .get_os_ratings_for_matches(match_ids, guild_id)
            .map(|ratings| {
                ratings
                    .into_iter()
                    .map(|(match_id, ratings)| {
                        (
                            match_id,
                            RatingAnalysisHistoricalRatings {
                                team1: ratings.team1,
                                team2: ratings.team2,
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|error| RatingAnalysisRepositoryError::Replay(error.to_string()))
    }

    pub fn pending_replay_reason(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<String>, RatingAnalysisRepositoryError> {
        let guild_id = PlayerRepository::normalize_guild_id(guild_id);
        open_runtime_connection(&self.path)?
            .query_row(
                "SELECT reason FROM openskill_replay_jobs WHERE guild_id=?1",
                [guild_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
}

fn sqlite_text(value: rusqlite::types::Value) -> String {
    match value {
        rusqlite::types::Value::Null => String::new(),
        rusqlite::types::Value::Integer(value) => value.to_string(),
        rusqlite::types::Value::Real(value) => value.to_string(),
        rusqlite::types::Value::Text(value) => value,
        rusqlite::types::Value::Blob(value) => String::from_utf8_lossy(&value).into_owned(),
    }
}

#[cfg(test)]
mod tests;
