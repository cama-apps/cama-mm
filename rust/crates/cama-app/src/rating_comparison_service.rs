//! Rating-system comparison orchestration without a live repository or Discord.
//!
//! This module coordinates already-loaded historical match rows and immutable
//! pre-match OpenSkill snapshots through typed ports; runtime and persistence
//! adapters remain outside the application layer.

use std::collections::{BTreeMap, HashMap};

use cama_domain::openskill::{CamaOpenSkillSystem, Rating};

pub type GuildId = i64;
pub type MatchId = i64;
pub type PlayerId = i64;

const MINIMUM_COMPLETE_MATCHES: usize = 10;
const LOG_LOSS_EPSILON: f64 = 0.001;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoricalRatingMatch {
    pub match_id: MatchId,
    pub match_date: i64,
    pub winning_team: i32,
    pub expected_radiant_win_prob: Option<f64>,
    pub team1_players: Vec<PlayerId>,
    pub team2_players: Vec<PlayerId>,
    pub openskill_radiant_win_prob: Option<f64>,
    pub openskill_raw_radiant_win_prob: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoricalOpenSkillRatings {
    pub team1: Vec<Rating>,
    pub team2: Vec<Rating>,
}

/// Bulk historical-match boundary supplied by a persistence adapter at
/// runtime. Implementations must return immutable pre-match rating snapshots,
/// never current player ratings.
pub trait RatingComparisonMatchPort {
    fn all_matches_with_predictions(
        &mut self,
        guild_id: Option<GuildId>,
    ) -> Vec<HistoricalRatingMatch>;

    fn openskill_ratings_for_matches(
        &mut self,
        match_ids: &[MatchId],
        guild_id: Option<GuildId>,
    ) -> HashMap<MatchId, HistoricalOpenSkillRatings>;
}

/// Retained constructor dependency from the Python service. The historical
/// comparison deliberately never invokes this current-rating lookup.
pub trait CurrentOpenSkillPredictionPort {
    fn predict_from_current_ratings(
        &mut self,
        team1_players: &[PlayerId],
        team2_players: &[PlayerId],
        guild_id: Option<GuildId>,
    ) -> f64;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalibrationBucket {
    pub predicted: f64,
    pub actual_wins: usize,
    pub count: usize,
    pub avg_predicted: f64,
    pub actual_rate: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingSystemStats {
    pub name: String,
    pub total_predictions: usize,
    pub brier_score: f64,
    pub accuracy: f64,
    pub calibration_buckets: BTreeMap<&'static str, CalibrationBucket>,
    pub log_loss: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingComparisonMatchData {
    pub match_id: MatchId,
    pub match_date: i64,
    pub radiant_won: bool,
    pub glicko_radiant_prob: f64,
    pub openskill_radiant_prob: f64,
    pub raw_openskill_radiant_prob: f64,
    pub glicko_correct: bool,
    pub openskill_correct: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingComparisonResult {
    pub glicko: RatingSystemStats,
    pub openskill: RatingSystemStats,
    pub matches_analyzed: usize,
    pub match_data: Vec<RatingComparisonMatchData>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PredictionSystem {
    Glicko,
    OpenSkill,
}

pub struct RatingComparisonService<Matches, CurrentPredictions> {
    matches: Matches,
    current_predictions: CurrentPredictions,
    openskill: CamaOpenSkillSystem,
}

impl<Matches, CurrentPredictions> RatingComparisonService<Matches, CurrentPredictions>
where
    Matches: RatingComparisonMatchPort,
    CurrentPredictions: CurrentOpenSkillPredictionPort,
{
    #[must_use]
    pub fn new(matches: Matches, current_predictions: CurrentPredictions) -> Self {
        Self::with_openskill(matches, current_predictions, CamaOpenSkillSystem::default())
    }

    /// Construct the comparison service with the deployment-configured
    /// OpenSkill model used for legacy pre-match snapshot fallback.
    #[must_use]
    pub const fn with_openskill(
        matches: Matches,
        current_predictions: CurrentPredictions,
        openskill: CamaOpenSkillSystem,
    ) -> Self {
        Self {
            matches,
            current_predictions,
            openskill,
        }
    }

    #[must_use]
    pub const fn match_port(&self) -> &Matches {
        &self.matches
    }

    #[must_use]
    pub const fn current_prediction_port(&self) -> &CurrentPredictions {
        &self.current_predictions
    }

    /// Compare Glicko and immutable historical OpenSkill predictions. Fewer
    /// than ten complete rows intentionally suppress the report.
    pub fn analyze_rating_systems(
        &mut self,
        guild_id: Option<GuildId>,
    ) -> Option<RatingComparisonResult> {
        let matches = self.matches.all_matches_with_predictions(guild_id);
        if matches.is_empty() {
            return None;
        }

        let snapshot_match_ids: Vec<_> = matches
            .iter()
            .filter(|historical_match| {
                historical_match.expected_radiant_win_prob.is_some()
                    && !historical_match.team1_players.is_empty()
                    && !historical_match.team2_players.is_empty()
            })
            .map(|historical_match| historical_match.match_id)
            .collect();
        let openskill_ratings = self
            .matches
            .openskill_ratings_for_matches(&snapshot_match_ids, guild_id);

        let mut match_data = Vec::new();
        for historical_match in matches {
            let Some(glicko_radiant_prob) = historical_match.expected_radiant_win_prob else {
                continue;
            };
            if historical_match.team1_players.is_empty()
                || historical_match.team2_players.is_empty()
            {
                continue;
            }

            let (openskill_radiant_prob, raw_openskill_radiant_prob) = match (
                historical_match.openskill_radiant_win_prob,
                historical_match.openskill_raw_radiant_win_prob,
            ) {
                (Some(calibrated), Some(raw)) => (calibrated, raw),
                _ => {
                    let ratings = openskill_ratings
                        .get(&historical_match.match_id)
                        .cloned()
                        .unwrap_or_default();
                    if ratings.team1.len() != historical_match.team1_players.len()
                        || ratings.team2.len() != historical_match.team2_players.len()
                    {
                        continue;
                    }
                    let raw = self
                        .openskill
                        .os_predict_win_probability(&ratings.team1, &ratings.team2);
                    let calibrated = self
                        .openskill
                        .calibrate_win_probability(raw, None)
                        .expect("default OpenSkill temperature is valid");
                    (calibrated, raw)
                }
            };

            let radiant_won = historical_match.winning_team == 1;
            match_data.push(RatingComparisonMatchData {
                match_id: historical_match.match_id,
                match_date: historical_match.match_date,
                radiant_won,
                glicko_radiant_prob,
                openskill_radiant_prob,
                raw_openskill_radiant_prob,
                glicko_correct: (glicko_radiant_prob >= 0.5) == radiant_won,
                openskill_correct: (openskill_radiant_prob >= 0.5) == radiant_won,
            });
        }

        if match_data.len() < MINIMUM_COMPLETE_MATCHES {
            return None;
        }

        let glicko =
            Self::calculate_system_stats("Glicko-2", &match_data, PredictionSystem::Glicko);
        let openskill =
            Self::calculate_system_stats("OpenSkill", &match_data, PredictionSystem::OpenSkill);
        Some(RatingComparisonResult {
            glicko,
            openskill,
            matches_analyzed: match_data.len(),
            match_data,
        })
    }

    #[must_use]
    pub fn calculate_system_stats(
        name: impl Into<String>,
        match_data: &[RatingComparisonMatchData],
        prediction_system: PredictionSystem,
    ) -> RatingSystemStats {
        let name = name.into();
        if match_data.is_empty() {
            return RatingSystemStats {
                name,
                total_predictions: 0,
                brier_score: 0.25,
                accuracy: 0.5,
                calibration_buckets: BTreeMap::new(),
                log_loss: 1.0,
            };
        }

        let mut buckets: BTreeMap<_, _> = BUCKET_KEYS
            .into_iter()
            .map(|key| (key, CalibrationBucket::default()))
            .collect();
        let mut brier_sum = 0.0;
        let mut correct_count = 0;
        let mut log_loss_sum = 0.0;

        for datum in match_data {
            let probability = match prediction_system {
                PredictionSystem::Glicko => datum.glicko_radiant_prob,
                PredictionSystem::OpenSkill => datum.openskill_radiant_prob,
            };
            let outcome = f64::from(datum.radiant_won);
            brier_sum += (probability - outcome).powi(2);
            correct_count += usize::from((probability >= 0.5) == datum.radiant_won);

            let clamped = probability.clamp(LOG_LOSS_EPSILON, 1.0 - LOG_LOSS_EPSILON);
            log_loss_sum -= if datum.radiant_won {
                clamped.ln()
            } else {
                (1.0 - clamped).ln()
            };

            let bucket = buckets
                .get_mut(Self::bucket_key(probability))
                .expect("every probability bucket is initialized");
            bucket.predicted += probability;
            bucket.actual_wins += usize::from(datum.radiant_won);
            bucket.count += 1;
        }

        for bucket in buckets.values_mut() {
            if bucket.count != 0 {
                bucket.avg_predicted = bucket.predicted / bucket.count as f64;
                bucket.actual_rate = bucket.actual_wins as f64 / bucket.count as f64;
            }
        }

        let total_predictions = match_data.len();
        RatingSystemStats {
            name,
            total_predictions,
            brier_score: brier_sum / total_predictions as f64,
            accuracy: correct_count as f64 / total_predictions as f64,
            calibration_buckets: buckets,
            log_loss: log_loss_sum / total_predictions as f64,
        }
    }

    #[must_use]
    pub const fn bucket_key(probability: f64) -> &'static str {
        if probability < 0.1 {
            "0-10%"
        } else if probability < 0.2 {
            "10-20%"
        } else if probability < 0.3 {
            "20-30%"
        } else if probability < 0.4 {
            "30-40%"
        } else if probability < 0.5 {
            "40-50%"
        } else if probability < 0.6 {
            "50-60%"
        } else if probability < 0.7 {
            "60-70%"
        } else if probability < 0.8 {
            "70-80%"
        } else if probability < 0.9 {
            "80-90%"
        } else {
            "90-100%"
        }
    }
}

const BUCKET_KEYS: [&str; 10] = [
    "0-10%", "10-20%", "20-30%", "30-40%", "40-50%", "50-60%", "60-70%", "70-80%", "80-90%",
    "90-100%",
];

#[cfg(test)]
mod tests;
