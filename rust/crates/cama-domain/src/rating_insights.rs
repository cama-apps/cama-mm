//! Pure rating-system analytics and per-player calibration insights.
//!
//! The Python implementation accepts loosely typed dictionaries. This port
//! makes those snapshots explicit while retaining the same filtering,
//! threshold, ordering, and aggregation rules.

use std::collections::{HashMap, HashSet};

use crate::player::{OPENSKILL_DISPLAY_SCALE, Player};

/// Display-rank thresholds in descending order.
pub const RATING_BUCKETS: [(&str, f64); 8] = [
    ("Immortal", 1_355.0),
    ("Divine", 1_155.0),
    ("Ancient", 962.0),
    ("Legend", 770.0),
    ("Archon", 578.0),
    ("Crusader", 385.0),
    ("Guardian", 192.0),
    ("Herald", 0.0),
];

/// OpenSkill sigma at or below this value is considered calibrated.
pub const OPENSKILL_CALIBRATION_THRESHOLD: f64 = 4.0;

/// Temperature used by the deployed OpenSkill probability calibration.
pub const OPENSKILL_WIN_PROBABILITY_TEMPERATURE: f64 = 6.5;

const OPENSKILL_WIN_PROBABILITY_EPSILON: f64 = 1e-12;
const OPENSKILL_BETA: f64 = 25.0 / 6.0;

/// One persisted rating-history row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatingHistoryEntry {
    pub match_id: Option<i64>,
    pub team_number: Option<i32>,
    pub won: Option<bool>,
    pub rating_before: Option<f64>,
    pub rating: Option<f64>,
    pub rd_before: Option<f64>,
    pub expected_team_win_prob: Option<f64>,
    pub os_mu_before: Option<f64>,
    pub os_mu_after: Option<f64>,
    pub os_sigma_before: Option<f64>,
}

/// One match-level Glicko prediction and recorded winner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MatchPrediction {
    pub expected_radiant_win_prob: Option<f64>,
    pub winning_team: Option<i32>,
}

/// Aggregate quality metrics for binary match predictions.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PredictionQuality {
    pub count: usize,
    pub brier: Option<f64>,
    pub ece: Option<f64>,
    pub accuracy: Option<f64>,
    pub balance_rate: Option<f64>,
    pub upset_rate: Option<f64>,
}

/// Absolute rating movement summary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RatingMovement {
    pub count: usize,
    pub avg_delta: Option<f64>,
    pub median_delta: Option<f64>,
}

/// Radiant/Dire outcome summary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SideBalance {
    pub radiant_wins: usize,
    pub dire_wins: usize,
    pub total: usize,
    pub radiant_rate: Option<f64>,
    pub dire_rate: Option<f64>,
}

/// Movement split by calibration state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RatingStability {
    pub calibrated_avg_delta: Option<f64>,
    pub calibrated_count: usize,
    pub uncalibrated_avg_delta: Option<f64>,
    pub uncalibrated_count: usize,
    pub stability_ratio: Option<f64>,
}

/// A named count whose surrounding vector defines display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedCount {
    pub name: &'static str,
    pub count: usize,
}

/// One half of the team-composition comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamCompositionHalf {
    pub name: &'static str,
    pub wins: usize,
    pub total: usize,
    pub winrate: f64,
    pub avg_expected: f64,
    pub overperformance: f64,
    pub avg_gini: f64,
}

/// Team rating-spread analytics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TeamCompositionStats {
    pub halves: Vec<TeamCompositionHalf>,
    pub total_teams: usize,
    pub gini_correlation: Option<f64>,
}

/// Complete global calibration report.
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationStats {
    pub total_players: usize,
    pub match_count: usize,
    pub rated_players: usize,
    pub avg_games: Option<f64>,
    pub rating_buckets: Vec<NamedCount>,
    pub avg_rating: Option<f64>,
    pub median_rating: Option<f64>,
    pub rd_tiers: Vec<NamedCount>,
    pub top_rated: Vec<Player>,
    pub lowest_rated: Vec<Player>,
    pub most_calibrated: Vec<Player>,
    pub least_calibrated: Vec<Player>,
    pub highest_volatility: Vec<Player>,
    pub most_experienced: Vec<Player>,
    pub avg_drift: Option<f64>,
    pub median_drift: Option<f64>,
    pub biggest_gainers: Vec<(Player, f64)>,
    pub biggest_drops: Vec<(Player, f64)>,
    pub prediction_quality: PredictionQuality,
    pub glicko_prediction_quality: PredictionQuality,
    pub openskill_prediction_quality: PredictionQuality,
    pub rating_movement: RatingMovement,
    pub glicko_rating_movement: RatingMovement,
    pub openskill_rating_movement: RatingMovement,
    pub side_balance: SideBalance,
    pub rating_stability: RatingStability,
    pub glicko_rating_stability: RatingStability,
    pub openskill_rating_stability: RatingStability,
    pub team_composition: TeamCompositionStats,
    pub avg_certainty: Option<f64>,
    pub avg_rd: Option<f64>,
}

impl CalibrationStats {
    /// Return a display bucket count by its stable Python label.
    #[must_use]
    pub fn rating_bucket_count(&self, name: &str) -> Option<usize> {
        self.rating_buckets
            .iter()
            .find(|bucket| bucket.name == name)
            .map(|bucket| bucket.count)
    }

    /// Return a calibration-tier count by its stable Python label.
    #[must_use]
    pub fn rd_tier_count(&self, name: &str) -> Option<usize> {
        self.rd_tiers
            .iter()
            .find(|tier| tier.name == name)
            .map(|tier| tier.count)
    }
}

/// Converts a source MMR into the deliberately underseeded rating.
pub trait RatingSeedCalculator {
    fn seed_rating(&self, source_mmr: i64) -> f64;
}

/// Default deployed Glicko seed policy: clamp, subtract 500 MMR, then scale by 0.25.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultRatingSeedCalculator;

impl RatingSeedCalculator for DefaultRatingSeedCalculator {
    fn seed_rating(&self, source_mmr: i64) -> f64 {
        let capped = source_mmr.clamp(0, 12_000);
        let underseeded = (capped - 500).max(0);
        underseeded as f64 * 0.25
    }
}

/// OpenSkill ratings for both sides of one match.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenSkillMatchRatings {
    pub team1: Vec<(f64, f64)>,
    pub team2: Vec<(f64, f64)>,
}

/// Abstracts prediction so callers can use the production model or a deterministic test model.
pub trait OpenSkillPredictor {
    fn calibrated_win_probability(&self, team: &[(f64, f64)], opponent: &[(f64, f64)]) -> f64;
}

/// Pure implementation of the deployed two-team OpenSkill probability policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct CamaOpenSkillPredictor;

impl CamaOpenSkillPredictor {
    /// Raw Plackett-Luce two-team win probability.
    #[must_use]
    pub fn raw_win_probability(team: &[(f64, f64)], opponent: &[(f64, f64)]) -> f64 {
        if team.is_empty() || opponent.is_empty() {
            return 0.5;
        }

        let team_mu: f64 = team.iter().map(|(mu, _)| mu).sum();
        let opponent_mu: f64 = opponent.iter().map(|(mu, _)| mu).sum();
        let variance: f64 = team
            .iter()
            .chain(opponent)
            .map(|(_, sigma)| sigma * sigma)
            .sum();
        let denominator = (2.0 * OPENSKILL_BETA.powi(2) + variance).sqrt();
        normal_cdf((team_mu - opponent_mu) / denominator)
    }

    /// Apply the same log-odds temperature scaling as Python.
    #[must_use]
    pub fn calibrate_win_probability(probability: f64, temperature: f64) -> f64 {
        assert!(
            temperature > 0.0,
            "OpenSkill probability temperature must be positive"
        );
        let probability = probability.clamp(
            OPENSKILL_WIN_PROBABILITY_EPSILON,
            1.0 - OPENSKILL_WIN_PROBABILITY_EPSILON,
        );
        let scaled_log_odds = (probability / (1.0 - probability)).ln() / temperature;
        if scaled_log_odds >= 0.0 {
            let exp_negative = (-scaled_log_odds).exp();
            1.0 / (1.0 + exp_negative)
        } else {
            let exp_positive = scaled_log_odds.exp();
            exp_positive / (1.0 + exp_positive)
        }
    }
}

impl OpenSkillPredictor for CamaOpenSkillPredictor {
    fn calibrated_win_probability(&self, team: &[(f64, f64)], opponent: &[(f64, f64)]) -> f64 {
        Self::calibrate_win_probability(
            Self::raw_win_probability(team, opponent),
            OPENSKILL_WIN_PROBABILITY_TEMPERATURE,
        )
    }
}

/// Bulk source for pre-match OpenSkill snapshots.
pub trait OpenSkillMatchSource {
    fn get_os_ratings_for_matches(
        &mut self,
        match_ids: &[i64],
        guild_id: Option<i64>,
    ) -> HashMap<i64, OpenSkillMatchRatings>;
}

/// Calibration values shared by individual rating views.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerCalibration {
    pub percentile: Option<f64>,
    pub drift: Option<f64>,
    pub matches_with_predictions: Vec<RatingHistoryEntry>,
    pub actual_wins: usize,
    pub expected_wins: f64,
    pub overperformance: Option<f64>,
    pub favored_matches: Vec<RatingHistoryEntry>,
    pub underdog_matches: Vec<RatingHistoryEntry>,
    pub favored_wins: usize,
    pub underdog_wins: usize,
    pub last_5_delta: Option<f64>,
    pub openskill_last_5_delta: Option<f64>,
    pub streak: usize,
    pub streak_type: Option<&'static str>,
    pub upsets: Vec<(RatingHistoryEntry, f64)>,
    pub chokes: Vec<(RatingHistoryEntry, f64)>,
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

// Numerical Recipes' complementary-error-function approximation. Its maximum
// error is well below the analytics' 1e-6 contract and it avoids a math crate
// solely for the OpenSkill normal CDF.
fn normal_cdf(value: f64) -> f64 {
    let x = -value / 2.0_f64.sqrt();
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let erfc_positive = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87
                                        + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    let erfc = if x >= 0.0 {
        erfc_positive
    } else {
        2.0 - erfc_positive
    };
    0.5 * erfc
}

/// Convert RD to certainty percentage. Higher means more certain.
#[must_use]
pub fn rd_to_certainty(rd: f64) -> f64 {
    100.0 - ((rd / 350.0) * 100.0).min(100.0)
}

/// Return the display calibration tier for an RD value.
#[must_use]
pub fn get_rd_tier_name(rd: f64) -> &'static str {
    if rd <= 75.0 {
        "Locked In"
    } else if rd <= 150.0 {
        "Settling"
    } else if rd <= 250.0 {
        "Developing"
    } else {
        "Fresh"
    }
}

/// Compute adaptive-binned expected calibration error.
#[must_use]
pub fn compute_ece(prob_actual_pairs: &[(f64, i32)], max_bins: usize) -> Option<f64> {
    if prob_actual_pairs.is_empty() {
        return None;
    }

    let mut sorted_pairs = prob_actual_pairs.to_vec();
    sorted_pairs.sort_by(|left, right| left.0.total_cmp(&right.0));
    let count = sorted_pairs.len();
    let bins = max_bins.min(count);
    let mut ece = 0.0;
    for index in 0..bins {
        let start = index * count / bins;
        let end = (index + 1) * count / bins;
        let bucket = &sorted_pairs[start..end];
        if bucket.is_empty() {
            continue;
        }
        let avg_probability = bucket
            .iter()
            .map(|(probability, _)| probability)
            .sum::<f64>()
            / bucket.len() as f64;
        let actual_rate = bucket
            .iter()
            .map(|(_, actual)| f64::from(*actual))
            .sum::<f64>()
            / bucket.len() as f64;
        ece += bucket.len() as f64 / count as f64 * (avg_probability - actual_rate).abs();
    }
    Some(ece)
}

/// Aggregate match-level prediction quality.
#[must_use]
pub fn compute_prediction_quality(predictions: &[MatchPrediction]) -> PredictionQuality {
    let mut pairs = Vec::new();
    let mut brier_scores = Vec::new();
    let mut correct = 0;
    let mut balanced = 0;
    let mut upsets = 0;
    let mut upset_eligible = 0;

    for prediction in predictions {
        let (Some(probability), Some(winning_team @ (1 | 2))) = (
            prediction.expected_radiant_win_prob,
            prediction.winning_team,
        ) else {
            continue;
        };
        let actual = i32::from(winning_team == 1);
        pairs.push((probability, actual));
        brier_scores.push((probability - f64::from(actual)).powi(2));
        let predicted = i32::from(probability >= 0.5);
        correct += usize::from(predicted == actual);
        balanced += usize::from((0.45..=0.55).contains(&probability));
        if probability >= 0.6 || probability <= 0.4 {
            upset_eligible += 1;
            upsets += usize::from(
                (probability >= 0.6 && actual == 0) || (probability <= 0.4 && actual == 1),
            );
        }
    }

    let count = brier_scores.len();
    PredictionQuality {
        count,
        brier: mean(&brier_scores),
        ece: compute_ece(&pairs, 5),
        accuracy: (count != 0).then(|| correct as f64 / count as f64),
        balance_rate: (count != 0).then(|| balanced as f64 / count as f64),
        upset_rate: (upset_eligible != 0).then(|| upsets as f64 / upset_eligible as f64),
    }
}

#[derive(Default)]
struct OpenSkillMatchGroup<'a> {
    team1: Vec<&'a RatingHistoryEntry>,
    team2: Vec<&'a RatingHistoryEntry>,
}

/// Reconstruct OpenSkill predictions from complete pre-match team snapshots.
#[must_use]
pub fn compute_openskill_prediction_quality(history: &[RatingHistoryEntry]) -> PredictionQuality {
    if history.is_empty() {
        return PredictionQuality::default();
    }

    // Python dictionaries preserve the first-seen match order. Keep a vector
    // plus an index rather than iterating a HashMap so equal-probability ECE
    // buckets remain deterministic.
    let mut match_indexes = HashMap::new();
    let mut matches: Vec<(i64, OpenSkillMatchGroup<'_>)> = Vec::new();
    for entry in history {
        let (Some(match_id), Some(team_number @ (1 | 2))) = (entry.match_id, entry.team_number)
        else {
            continue;
        };
        let index = *match_indexes.entry(match_id).or_insert_with(|| {
            matches.push((match_id, OpenSkillMatchGroup::default()));
            matches.len() - 1
        });
        if team_number == 1 {
            matches[index].1.team1.push(entry);
        } else {
            matches[index].1.team2.push(entry);
        }
    }

    let predictor = CamaOpenSkillPredictor;
    let mut predictions = Vec::new();
    for (_, teams) in matches {
        if teams.team1.len() != 5 || teams.team2.len() != 5 {
            continue;
        }
        let team1: Option<Vec<_>> = teams
            .team1
            .iter()
            .map(|entry| Some((entry.os_mu_before?, entry.os_sigma_before?)))
            .collect();
        let team2: Option<Vec<_>> = teams
            .team2
            .iter()
            .map(|entry| Some((entry.os_mu_before?, entry.os_sigma_before?)))
            .collect();
        let (Some(team1), Some(team2)) = (team1, team2) else {
            continue;
        };
        let team1_won = teams.team1[0].won.unwrap_or(false);
        let team2_won = teams.team2[0].won.unwrap_or(false);
        if team1_won == team2_won {
            continue;
        }
        predictions.push(MatchPrediction {
            expected_radiant_win_prob: Some(predictor.calibrated_win_probability(&team1, &team2)),
            winning_team: Some(if team1_won { 1 } else { 2 }),
        });
    }
    compute_prediction_quality(&predictions)
}

/// Summarize absolute Glicko movement.
#[must_use]
pub fn compute_rating_movement(history: &[RatingHistoryEntry]) -> RatingMovement {
    let deltas: Vec<_> = history
        .iter()
        .filter_map(|entry| Some((entry.rating? - entry.rating_before?).abs()))
        .collect();
    RatingMovement {
        count: deltas.len(),
        avg_delta: mean(&deltas),
        median_delta: median(&deltas),
    }
}

/// Summarize absolute OpenSkill movement on the shared display scale.
#[must_use]
pub fn compute_openskill_rating_movement(history: &[RatingHistoryEntry]) -> RatingMovement {
    let deltas: Vec<_> = history
        .iter()
        .filter_map(|entry| {
            Some((entry.os_mu_after? - entry.os_mu_before?).abs() * OPENSKILL_DISPLAY_SCALE)
        })
        .collect();
    RatingMovement {
        count: deltas.len(),
        avg_delta: mean(&deltas),
        median_delta: median(&deltas),
    }
}

/// Compute Radiant/Dire result balance.
#[must_use]
pub fn compute_side_balance(predictions: &[MatchPrediction]) -> SideBalance {
    let radiant_wins = predictions
        .iter()
        .filter(|entry| entry.winning_team == Some(1))
        .count();
    let dire_wins = predictions
        .iter()
        .filter(|entry| entry.winning_team == Some(2))
        .count();
    let total = radiant_wins + dire_wins;
    SideBalance {
        radiant_wins,
        dire_wins,
        total,
        radiant_rate: (total != 0).then(|| radiant_wins as f64 / total as f64),
        dire_rate: (total != 0).then(|| dire_wins as f64 / total as f64),
    }
}

fn stability_from_deltas(calibrated: &[f64], uncalibrated: &[f64]) -> RatingStability {
    let calibrated_average = mean(calibrated);
    let uncalibrated_average = mean(uncalibrated);
    let stability_ratio = match (calibrated_average, uncalibrated_average) {
        (Some(calibrated), Some(uncalibrated)) if uncalibrated > 0.0 => {
            Some(calibrated / uncalibrated)
        }
        _ => None,
    };
    RatingStability {
        calibrated_avg_delta: calibrated_average,
        calibrated_count: calibrated.len(),
        uncalibrated_avg_delta: uncalibrated_average,
        uncalibrated_count: uncalibrated.len(),
        stability_ratio,
    }
}

/// Compare Glicko movement for RD at/below and above 150.
#[must_use]
pub fn compute_rating_stability(history: &[RatingHistoryEntry]) -> RatingStability {
    let mut calibrated = Vec::new();
    let mut uncalibrated = Vec::new();
    for entry in history {
        let (Some(before), Some(after), Some(rd)) =
            (entry.rating_before, entry.rating, entry.rd_before)
        else {
            continue;
        };
        let target = if rd <= 150.0 {
            &mut calibrated
        } else {
            &mut uncalibrated
        };
        target.push((after - before).abs());
    }
    stability_from_deltas(&calibrated, &uncalibrated)
}

/// Compare OpenSkill movement above and below its sigma calibration threshold.
#[must_use]
pub fn compute_openskill_rating_stability(history: &[RatingHistoryEntry]) -> RatingStability {
    let mut calibrated = Vec::new();
    let mut uncalibrated = Vec::new();
    for entry in history {
        let (Some(before), Some(after), Some(sigma)) =
            (entry.os_mu_before, entry.os_mu_after, entry.os_sigma_before)
        else {
            continue;
        };
        let target = if sigma <= OPENSKILL_CALIBRATION_THRESHOLD {
            &mut calibrated
        } else {
            &mut uncalibrated
        };
        target.push((after - before).abs() * OPENSKILL_DISPLAY_SCALE);
    }
    stability_from_deltas(&calibrated, &uncalibrated)
}

/// Compute the Gini coefficient of a rating distribution.
#[must_use]
pub fn gini_coefficient(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean_value = values.iter().sum::<f64>() / values.len() as f64;
    if mean_value <= 0.0 {
        return 0.0;
    }
    let absolute_differences: f64 = values
        .iter()
        .flat_map(|left| values.iter().map(move |right| (left - right).abs()))
        .sum();
    absolute_differences / (2.0 * (values.len() * values.len()) as f64 * mean_value)
}

/// Compute Pearson's correlation, requiring at least three non-constant points.
#[must_use]
pub fn pearson_r(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() < 3 {
        return None;
    }
    let mean_x = xs.iter().sum::<f64>() / xs.len() as f64;
    let mean_y = ys.iter().sum::<f64>() / ys.len() as f64;
    let numerator: f64 = xs
        .iter()
        .zip(ys)
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum();
    let denominator_x = xs.iter().map(|x| (x - mean_x).powi(2)).sum::<f64>().sqrt();
    let denominator_y = ys.iter().map(|y| (y - mean_y).powi(2)).sum::<f64>().sqrt();
    if denominator_x == 0.0 || denominator_y == 0.0 {
        None
    } else {
        Some(numerator / (denominator_x * denominator_y))
    }
}

#[derive(Clone, Copy)]
struct TeamDatum {
    gini: f64,
    expected: f64,
    won: bool,
    overperformance: f64,
}

fn aggregate_team_half(items: &[TeamDatum], name: &'static str) -> TeamCompositionHalf {
    let wins = items.iter().filter(|item| item.won).count();
    let total = items.len();
    let winrate = if total == 0 {
        0.0
    } else {
        wins as f64 / total as f64
    };
    let avg_expected = items.iter().map(|item| item.expected).sum::<f64>() / total as f64;
    let avg_gini = items.iter().map(|item| item.gini).sum::<f64>() / total as f64;
    TeamCompositionHalf {
        name,
        wins,
        total,
        winrate,
        avg_expected,
        overperformance: winrate - avg_expected,
        avg_gini,
    }
}

/// Analyze how within-team rating spread correlates with overperformance.
#[must_use]
pub fn compute_team_composition_stats(history: &[RatingHistoryEntry]) -> TeamCompositionStats {
    if history.is_empty() {
        return TeamCompositionStats::default();
    }

    let mut indexes = HashMap::new();
    let mut teams: Vec<((i64, i32), Vec<&RatingHistoryEntry>)> = Vec::new();
    for entry in history {
        let (Some(match_id), Some(team_number)) = (entry.match_id, entry.team_number) else {
            continue;
        };
        let key = (match_id, team_number);
        let index = *indexes.entry(key).or_insert_with(|| {
            teams.push((key, Vec::new()));
            teams.len() - 1
        });
        teams[index].1.push(entry);
    }

    let mut team_data = Vec::new();
    for (_, entries) in teams {
        if entries.len() != 5 {
            continue;
        }
        let ratings: Option<Vec<_>> = entries.iter().map(|entry| entry.rating_before).collect();
        let Some(ratings) = ratings else {
            continue;
        };
        let (Some(expected), Some(won)) = (entries[0].expected_team_win_prob, entries[0].won)
        else {
            continue;
        };
        team_data.push(TeamDatum {
            gini: gini_coefficient(&ratings),
            expected,
            won,
            overperformance: f64::from(won) - expected,
        });
    }

    let total_teams = team_data.len();
    if total_teams == 0 {
        return TeamCompositionStats::default();
    }
    let ginis: Vec<_> = team_data.iter().map(|team| team.gini).collect();
    let overperformances: Vec<_> = team_data.iter().map(|team| team.overperformance).collect();
    let gini_correlation = pearson_r(&ginis, &overperformances);
    if total_teams < 6 {
        return TeamCompositionStats {
            halves: Vec::new(),
            total_teams,
            gini_correlation,
        };
    }

    team_data.sort_by(|left, right| left.gini.total_cmp(&right.gini));
    let middle = total_teams / 2;
    TeamCompositionStats {
        halves: vec![
            aggregate_team_half(&team_data[..middle], "Similar ratings"),
            aggregate_team_half(&team_data[middle..], "Mixed ratings"),
        ],
        total_teams,
        gini_correlation,
    }
}

/// Compute the global calibration report from typed snapshots.
#[must_use]
pub fn compute_calibration_stats(
    players: &[Player],
    match_count: usize,
    match_predictions: &[MatchPrediction],
    history: &[RatingHistoryEntry],
) -> CalibrationStats {
    let rated_players: Vec<_> = players
        .iter()
        .filter(|player| player.glicko_rating.is_some())
        .collect();
    let rating_values: Vec<_> = rated_players
        .iter()
        .filter_map(|player| player.glicko_rating)
        .collect();
    let rd_values: Vec<_> = rated_players
        .iter()
        .map(|player| player.glicko_rd.unwrap_or(350.0))
        .collect();

    let mut rating_buckets: Vec<_> = RATING_BUCKETS
        .iter()
        .map(|(name, _)| NamedCount { name, count: 0 })
        .collect();
    for rating in &rating_values {
        if let Some(index) = RATING_BUCKETS
            .iter()
            .position(|(_, threshold)| rating >= threshold)
        {
            rating_buckets[index].count += 1;
        }
    }
    let mut rd_tiers = vec![
        NamedCount {
            name: "Locked In",
            count: 0,
        },
        NamedCount {
            name: "Settling",
            count: 0,
        },
        NamedCount {
            name: "Developing",
            count: 0,
        },
        NamedCount {
            name: "Fresh",
            count: 0,
        },
    ];
    for rd in &rd_values {
        let index = match get_rd_tier_name(*rd) {
            "Locked In" => 0,
            "Settling" => 1,
            "Developing" => 2,
            _ => 3,
        };
        rd_tiers[index].count += 1;
    }

    let total_games: Vec<_> = players
        .iter()
        .map(|player| (player.wins + player.losses) as f64)
        .collect();
    let mut top_rated = rated_players.clone();
    top_rated.sort_by(|left, right| {
        right
            .glicko_rating
            .unwrap_or(0.0)
            .total_cmp(&left.glicko_rating.unwrap_or(0.0))
    });
    let mut lowest_rated = rated_players.clone();
    lowest_rated.sort_by(|left, right| {
        left.glicko_rating
            .unwrap_or(0.0)
            .total_cmp(&right.glicko_rating.unwrap_or(0.0))
    });
    let mut most_calibrated = rated_players.clone();
    most_calibrated.sort_by(|left, right| {
        left.glicko_rd
            .unwrap_or(350.0)
            .total_cmp(&right.glicko_rd.unwrap_or(350.0))
    });
    let mut least_calibrated = rated_players.clone();
    least_calibrated.sort_by(|left, right| {
        right
            .glicko_rd
            .unwrap_or(350.0)
            .total_cmp(&left.glicko_rd.unwrap_or(350.0))
    });
    let mut highest_volatility = rated_players.clone();
    highest_volatility.sort_by(|left, right| {
        right
            .glicko_volatility
            .unwrap_or(0.06)
            .total_cmp(&left.glicko_volatility.unwrap_or(0.06))
    });
    let mut most_experienced: Vec<_> = players.iter().collect();
    most_experienced.sort_by_key(|player| std::cmp::Reverse(player.wins + player.losses));

    let seed_calculator = DefaultRatingSeedCalculator;
    let mut drifts: Vec<_> = rated_players
        .iter()
        .filter_map(|player| {
            let rating = player.glicko_rating?;
            let initial_mmr = player.initial_mmr?;
            Some((
                (*player).clone(),
                rating - seed_calculator.seed_rating(initial_mmr),
            ))
        })
        .collect();
    let drift_values: Vec<_> = drifts.iter().map(|(_, drift)| *drift).collect();
    drifts.sort_by(|left, right| right.1.total_cmp(&left.1));

    let glicko_prediction_quality = compute_prediction_quality(match_predictions);
    let rating_movement = compute_rating_movement(history);
    let rating_stability = compute_rating_stability(history);
    let avg_rd = mean(&rd_values);

    CalibrationStats {
        total_players: players.len(),
        match_count,
        rated_players: rated_players.len(),
        avg_games: mean(&total_games),
        rating_buckets,
        avg_rating: mean(&rating_values),
        median_rating: median(&rating_values),
        rd_tiers,
        top_rated: top_rated.into_iter().take(3).cloned().collect(),
        lowest_rated: lowest_rated.into_iter().take(3).cloned().collect(),
        most_calibrated: most_calibrated.into_iter().take(3).cloned().collect(),
        least_calibrated: least_calibrated.into_iter().take(3).cloned().collect(),
        highest_volatility: highest_volatility.into_iter().take(3).cloned().collect(),
        most_experienced: most_experienced.into_iter().take(3).cloned().collect(),
        avg_drift: mean(&drift_values),
        median_drift: median(&drift_values),
        biggest_gainers: drifts.iter().take(3).cloned().collect(),
        biggest_drops: drifts.iter().rev().take(3).cloned().collect(),
        prediction_quality: glicko_prediction_quality,
        glicko_prediction_quality,
        openskill_prediction_quality: compute_openskill_prediction_quality(history),
        rating_movement,
        glicko_rating_movement: rating_movement,
        openskill_rating_movement: compute_openskill_rating_movement(history),
        side_balance: compute_side_balance(match_predictions),
        rating_stability,
        glicko_rating_stability: rating_stability,
        openskill_rating_stability: compute_openskill_rating_stability(history),
        team_composition: compute_team_composition_stats(history),
        avg_certainty: avg_rd.map(rd_to_certainty),
        avg_rd,
    }
}

/// Compute the per-player values shared by the calibration and profile views.
#[must_use]
pub fn compute_player_calibration(
    player: &Player,
    history: &[RatingHistoryEntry],
    rated_players: &[Player],
    seed_calculator: &impl RatingSeedCalculator,
) -> PlayerCalibration {
    let percentile = player.glicko_rating.and_then(|rating| {
        (!rated_players.is_empty()).then(|| {
            let lower_count = rated_players
                .iter()
                .filter(|other| other.glicko_rating.unwrap_or(0.0) < rating)
                .count();
            lower_count as f64 / rated_players.len() as f64 * 100.0
        })
    });
    let drift = player
        .initial_mmr
        .zip(player.glicko_rating)
        .map(|(mmr, rating)| rating - seed_calculator.seed_rating(mmr));

    let matches_with_predictions: Vec<_> = history
        .iter()
        .filter(|entry| entry.expected_team_win_prob.is_some())
        .cloned()
        .collect();
    let actual_wins = matches_with_predictions
        .iter()
        .filter(|entry| entry.won == Some(true))
        .count();
    let expected_wins = matches_with_predictions
        .iter()
        .map(|entry| entry.expected_team_win_prob.unwrap_or(0.0))
        .sum::<f64>();
    let overperformance =
        (!matches_with_predictions.is_empty()).then_some(actual_wins as f64 - expected_wins);
    let favored_matches: Vec<_> = matches_with_predictions
        .iter()
        .filter(|entry| entry.expected_team_win_prob.unwrap_or(0.0) >= 0.55)
        .cloned()
        .collect();
    let underdog_matches: Vec<_> = matches_with_predictions
        .iter()
        .filter(|entry| entry.expected_team_win_prob.unwrap_or(0.0) <= 0.45)
        .cloned()
        .collect();
    let favored_wins = favored_matches
        .iter()
        .filter(|entry| entry.won == Some(true))
        .count();
    let underdog_wins = underdog_matches
        .iter()
        .filter(|entry| entry.won == Some(true))
        .count();

    // Python slices to five rows before filtering out rows for the other
    // rating system. Do not pull older snapshots into this window.
    let recent_glicko: Vec<_> = history
        .iter()
        .take(5)
        .filter(|entry| entry.rating.is_some())
        .collect();
    let last_5_delta = (recent_glicko.len() >= 2).then(|| {
        let newest = recent_glicko[0].rating.unwrap_or_default();
        let oldest = recent_glicko[recent_glicko.len() - 1];
        newest - oldest.rating_before.or(oldest.rating).unwrap_or_default()
    });
    let recent_openskill: Vec<_> = history
        .iter()
        .take(5)
        .filter(|entry| entry.os_mu_after.is_some())
        .collect();
    let openskill_last_5_delta = (recent_openskill.len() >= 2).then(|| {
        let newest = recent_openskill[0].os_mu_after.unwrap_or_default();
        let oldest = recent_openskill[recent_openskill.len() - 1];
        (newest
            - oldest
                .os_mu_before
                .or(oldest.os_mu_after)
                .unwrap_or_default())
            * OPENSKILL_DISPLAY_SCALE
    });

    let mut streak = 0;
    let mut streak_type = None;
    for won in history.iter().filter_map(|entry| entry.won) {
        match streak_type {
            None => {
                streak_type = Some(if won { "W" } else { "L" });
                streak = 1;
            }
            Some("W") if won => streak += 1,
            Some("L") if !won => streak += 1,
            _ => break,
        }
    }

    let mut upsets: Vec<_> = matches_with_predictions
        .iter()
        .filter_map(|entry| {
            let probability = entry.expected_team_win_prob?;
            let python_truthy_probability = if probability == 0.0 { 0.5 } else { probability };
            (entry.won == Some(true) && python_truthy_probability < 0.45)
                .then(|| (entry.clone(), probability))
        })
        .collect();
    upsets.sort_by(|left, right| left.1.total_cmp(&right.1));
    let mut chokes: Vec<_> = matches_with_predictions
        .iter()
        .filter_map(|entry| {
            let probability = entry.expected_team_win_prob?;
            let python_truthy_probability = if probability == 0.0 { 0.5 } else { probability };
            (entry.won != Some(true) && python_truthy_probability > 0.55)
                .then(|| (entry.clone(), probability))
        })
        .collect();
    chokes.sort_by(|left, right| right.1.total_cmp(&left.1));

    PlayerCalibration {
        percentile,
        drift,
        matches_with_predictions,
        actual_wins,
        expected_wins,
        overperformance,
        favored_matches,
        underdog_matches,
        favored_wins,
        underdog_wins,
        last_5_delta,
        openskill_last_5_delta,
        streak,
        streak_type,
        upsets,
        chokes,
    }
}

fn os_win_probability_from_ratings(
    predictor: &impl OpenSkillPredictor,
    ratings: &OpenSkillMatchRatings,
    team_number: Option<i32>,
) -> Option<f64> {
    if ratings.team1.is_empty() || ratings.team2.is_empty() {
        return None;
    }
    match team_number {
        Some(1) => Some(predictor.calibrated_win_probability(&ratings.team1, &ratings.team2)),
        Some(2) => Some(predictor.calibrated_win_probability(&ratings.team2, &ratings.team1)),
        _ => None,
    }
}

/// Bulk-load OpenSkill snapshots once and return aligned side probabilities.
#[must_use]
pub fn get_os_win_probabilities(
    match_source: Option<&mut impl OpenSkillMatchSource>,
    predictor: &impl OpenSkillPredictor,
    match_sides: &[(Option<i64>, Option<i32>)],
    guild_id: Option<i64>,
) -> Vec<Option<f64>> {
    let mut seen = HashSet::new();
    let match_ids: Vec<_> = match_sides
        .iter()
        .filter_map(|(match_id, _)| match_id.filter(|id| *id != 0))
        .filter(|match_id| seen.insert(*match_id))
        .collect();
    let Some(match_source) = match_source else {
        return vec![None; match_sides.len()];
    };
    if match_ids.is_empty() {
        return vec![None; match_sides.len()];
    }
    let ratings_by_match = match_source.get_os_ratings_for_matches(&match_ids, guild_id);
    match_sides
        .iter()
        .map(|(match_id, team_number)| {
            let match_id = match_id.filter(|id| *id != 0)?;
            let ratings = ratings_by_match.get(&match_id)?;
            os_win_probability_from_ratings(predictor, ratings, *team_number)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual}"
        );
    }

    #[derive(Default)]
    struct MatchSource {
        calls: Vec<(Vec<i64>, Option<i64>)>,
    }

    impl OpenSkillMatchSource for MatchSource {
        fn get_os_ratings_for_matches(
            &mut self,
            match_ids: &[i64],
            guild_id: Option<i64>,
        ) -> HashMap<i64, OpenSkillMatchRatings> {
            self.calls.push((match_ids.to_vec(), guild_id));
            HashMap::from([
                (
                    11,
                    OpenSkillMatchRatings {
                        team1: vec![(60.0, 5.0)],
                        team2: vec![(40.0, 5.0)],
                    },
                ),
                (
                    12,
                    OpenSkillMatchRatings {
                        team1: vec![(55.0, 5.0)],
                        team2: Vec::new(),
                    },
                ),
            ])
        }
    }

    struct FirstMuPredictor;

    impl OpenSkillPredictor for FirstMuPredictor {
        fn calibrated_win_probability(&self, team: &[(f64, f64)], _opponent: &[(f64, f64)]) -> f64 {
            team[0].0 / 100.0
        }
    }

    #[test]
    fn test_get_os_win_probabilities_bulk_loads_once_and_preserves_alignment() {
        let mut source = MatchSource::default();
        let probabilities = get_os_win_probabilities(
            Some(&mut source),
            &FirstMuPredictor,
            &[
                (Some(11), Some(1)),
                (Some(12), Some(1)),
                (Some(11), Some(2)),
                (None, Some(1)),
            ],
            Some(77),
        );

        assert_eq!(probabilities, vec![Some(0.6), None, Some(0.4), None]);
        assert_eq!(source.calls, vec![(vec![11, 12], Some(77))]);
    }

    #[test]
    fn test_compute_calibration_stats_with_predictions_and_drift() {
        let players = vec![
            Player {
                glicko_rating: Some(1_355.0),
                glicko_rd: Some(50.0),
                glicko_volatility: Some(0.08),
                wins: 10,
                initial_mmr: Some(5_000),
                ..Player::new("Immortal")
            },
            Player {
                glicko_rating: Some(800.0),
                glicko_rd: Some(200.0),
                glicko_volatility: Some(0.05),
                wins: 5,
                losses: 5,
                initial_mmr: Some(4_000),
                ..Player::new("Legend")
            },
            Player {
                glicko_rating: Some(300.0),
                glicko_rd: Some(300.0),
                glicko_volatility: Some(0.07),
                ..Player::new("Guardian")
            },
        ];
        let predictions = [
            MatchPrediction {
                expected_radiant_win_prob: Some(0.7),
                winning_team: Some(1),
            },
            MatchPrediction {
                expected_radiant_win_prob: Some(0.3),
                winning_team: Some(1),
            },
        ];
        let history = [
            RatingHistoryEntry {
                rating_before: Some(1_000.0),
                rating: Some(1_020.0),
                ..RatingHistoryEntry::default()
            },
            RatingHistoryEntry {
                rating_before: Some(1_020.0),
                rating: Some(1_010.0),
                ..RatingHistoryEntry::default()
            },
        ];

        let stats = compute_calibration_stats(&players, 12, &predictions, &history);

        assert_eq!(stats.rating_bucket_count("Immortal"), Some(1));
        assert_eq!(stats.rating_bucket_count("Legend"), Some(1));
        assert_eq!(stats.rating_bucket_count("Guardian"), Some(1));
        assert_eq!(stats.rd_tier_count("Locked In"), Some(1));
        assert_eq!(stats.rd_tier_count("Developing"), Some(1));
        assert_eq!(stats.rd_tier_count("Fresh"), Some(1));
        close(stats.avg_certainty.unwrap(), 47.6, 0.1);
        close(stats.avg_drift.unwrap(), 77.5, 1e-10);
        close(stats.median_drift.unwrap(), 77.5, 1e-10);
        assert_eq!(stats.prediction_quality.count, 2);
        close(stats.prediction_quality.brier.unwrap(), 0.29, 1e-10);
        close(stats.prediction_quality.ece.unwrap(), 0.5, 1e-10);
        close(stats.prediction_quality.accuracy.unwrap(), 0.5, 1e-10);
        close(stats.prediction_quality.balance_rate.unwrap(), 0.0, 1e-10);
        close(stats.prediction_quality.upset_rate.unwrap(), 0.5, 1e-10);
        assert_eq!(stats.glicko_prediction_quality, stats.prediction_quality);
        assert_eq!(stats.openskill_prediction_quality.count, 0);
        assert_eq!(stats.rating_movement.count, 2);
        close(stats.rating_movement.avg_delta.unwrap(), 15.0, 1e-10);
        close(stats.rating_movement.median_delta.unwrap(), 15.0, 1e-10);
        assert_eq!(stats.glicko_rating_movement, stats.rating_movement);
        assert_eq!(stats.openskill_rating_movement.count, 0);
    }

    #[test]
    fn test_compute_calibration_stats_reports_openskill_movement_and_stability_in_display_points() {
        let history = [
            RatingHistoryEntry {
                os_mu_before: Some(40.0),
                os_mu_after: Some(40.2),
                os_sigma_before: Some(3.0),
                ..RatingHistoryEntry::default()
            },
            RatingHistoryEntry {
                os_mu_before: Some(40.2),
                os_mu_after: Some(41.0),
                os_sigma_before: Some(6.0),
                ..RatingHistoryEntry::default()
            },
        ];
        let stats = compute_calibration_stats(&[], 0, &[], &history);
        let movement = stats.openskill_rating_movement;
        assert_eq!(movement.count, 2);
        close(movement.avg_delta.unwrap(), 25.0, 1e-10);
        close(movement.median_delta.unwrap(), 25.0, 1e-10);
        let stability = stats.openskill_rating_stability;
        close(stability.calibrated_avg_delta.unwrap(), 10.0, 1e-10);
        close(stability.uncalibrated_avg_delta.unwrap(), 40.0, 1e-10);
        close(stability.stability_ratio.unwrap(), 0.25, 1e-10);
    }

    #[test]
    fn test_compute_calibration_stats_openskill_prediction_quality_from_history() {
        let mut history = Vec::new();
        for match_id in 0..2 {
            let team1_won = match_id == 0;
            for _ in 0..5 {
                history.push(RatingHistoryEntry {
                    match_id: Some(match_id),
                    team_number: Some(1),
                    won: Some(team1_won),
                    rating_before: Some(1_000.0),
                    rating: Some(1_010.0),
                    os_mu_before: Some(60.0),
                    os_sigma_before: Some(4.0),
                    ..RatingHistoryEntry::default()
                });
                history.push(RatingHistoryEntry {
                    match_id: Some(match_id),
                    team_number: Some(2),
                    won: Some(!team1_won),
                    rating_before: Some(1_000.0),
                    rating: Some(990.0),
                    os_mu_before: Some(35.0),
                    os_sigma_before: Some(4.0),
                    ..RatingHistoryEntry::default()
                });
            }
        }

        let stats = compute_calibration_stats(&[], 2, &[], &history);
        let quality = stats.openskill_prediction_quality;
        let team1 = vec![(60.0, 4.0); 5];
        let team2 = vec![(35.0, 4.0); 5];
        let raw = CamaOpenSkillPredictor::raw_win_probability(&team1, &team2);
        let calibrated = CamaOpenSkillPredictor::calibrate_win_probability(
            raw,
            OPENSKILL_WIN_PROBABILITY_TEMPERATURE,
        );
        let expected_brier = ((calibrated - 1.0).powi(2) + calibrated.powi(2)) / 2.0;
        assert_eq!(quality.count, 2);
        close(quality.brier.unwrap(), expected_brier, 1e-10);
        assert!(quality.ece.is_some());
        close(quality.accuracy.unwrap(), 0.5, 1e-10);
        assert!(calibrated < raw);
    }

    #[test]
    fn test_player_calibration_treats_zero_rating_as_valid_seed_and_percentile() {
        let player = Player {
            glicko_rating: Some(0.0),
            initial_mmr: Some(500),
            ..Player::new("Zero")
        };
        let calibration = compute_player_calibration(
            &player,
            &[],
            &[
                player.clone(),
                Player {
                    glicko_rating: Some(100.0),
                    ..Player::new("Higher")
                },
            ],
            &DefaultRatingSeedCalculator,
        );
        close(calibration.drift.unwrap(), 0.0, 1e-10);
        close(calibration.percentile.unwrap(), 0.0, 1e-10);
    }

    fn history_row(rating: f64, rating_before: Option<f64>) -> RatingHistoryEntry {
        RatingHistoryEntry {
            rating: Some(rating),
            rating_before,
            ..RatingHistoryEntry::default()
        }
    }

    fn last_5_delta(history: &[RatingHistoryEntry]) -> Option<f64> {
        compute_player_calibration(
            &Player::new("p"),
            history,
            &[],
            &DefaultRatingSeedCalculator,
        )
        .last_5_delta
    }

    #[test]
    fn test_spans_five_games_with_longer_history() {
        let history = [
            history_row(1_550.0, Some(1_540.0)),
            history_row(1_540.0, Some(1_530.0)),
            history_row(1_530.0, Some(1_520.0)),
            history_row(1_520.0, Some(1_510.0)),
            history_row(1_510.0, Some(1_500.0)),
            history_row(1_500.0, Some(1_490.0)),
        ];
        assert_eq!(last_5_delta(&history), Some(50.0));
    }

    #[test]
    fn test_short_history_spans_all_games() {
        let history = [
            history_row(1_530.0, Some(1_520.0)),
            history_row(1_520.0, Some(1_510.0)),
            history_row(1_510.0, Some(1_500.0)),
        ];
        assert_eq!(last_5_delta(&history), Some(30.0));
    }

    #[test]
    fn test_legacy_rows_without_rating_before_fall_back() {
        let history = [
            history_row(1_550.0, None),
            history_row(1_540.0, None),
            history_row(1_530.0, None),
            history_row(1_520.0, None),
            history_row(1_510.0, None),
            history_row(1_500.0, None),
        ];
        assert_eq!(last_5_delta(&history), Some(40.0));
    }

    #[test]
    fn test_openskill_only_rows_do_not_distort_glicko_trend() {
        let history = [
            RatingHistoryEntry::default(),
            history_row(1_530.0, Some(1_520.0)),
            RatingHistoryEntry::default(),
            history_row(1_520.0, Some(1_510.0)),
        ];
        assert_eq!(last_5_delta(&history), Some(20.0));
    }

    #[test]
    fn test_glicko_trend_does_not_reach_past_five_games() {
        let history = [
            history_row(1_550.0, Some(1_540.0)),
            RatingHistoryEntry::default(),
            history_row(1_540.0, Some(1_530.0)),
            history_row(1_530.0, Some(1_520.0)),
            history_row(1_520.0, Some(1_510.0)),
            history_row(1_510.0, Some(1_500.0)),
        ];
        assert_eq!(last_5_delta(&history), Some(40.0));
    }

    #[test]
    fn test_one_glicko_snapshot_has_no_trend() {
        let history = [
            RatingHistoryEntry::default(),
            history_row(1_530.0, Some(1_520.0)),
            RatingHistoryEntry::default(),
        ];
        assert_eq!(last_5_delta(&history), None);
    }

    #[test]
    fn test_openskill_trend_uses_shared_display_scale() {
        let history = [
            RatingHistoryEntry {
                os_mu_after: Some(42.0),
                os_mu_before: Some(41.5),
                ..RatingHistoryEntry::default()
            },
            RatingHistoryEntry {
                os_mu_after: Some(41.5),
                os_mu_before: Some(41.0),
                ..RatingHistoryEntry::default()
            },
            RatingHistoryEntry {
                os_mu_after: Some(41.0),
                os_mu_before: Some(40.0),
                ..RatingHistoryEntry::default()
            },
        ];
        let calibration = compute_player_calibration(
            &Player::new("p"),
            &history,
            &[],
            &DefaultRatingSeedCalculator,
        );
        close(calibration.openskill_last_5_delta.unwrap(), 100.0, 1e-10);
    }

    #[test]
    fn test_player_streak_does_not_require_glicko_prediction_rows() {
        let history = [
            RatingHistoryEntry {
                won: Some(true),
                ..RatingHistoryEntry::default()
            },
            RatingHistoryEntry {
                won: Some(true),
                ..RatingHistoryEntry::default()
            },
            RatingHistoryEntry {
                won: Some(false),
                ..RatingHistoryEntry::default()
            },
        ];
        let calibration = compute_player_calibration(
            &Player::new("p"),
            &history,
            &[],
            &DefaultRatingSeedCalculator,
        );
        assert_eq!(calibration.streak_type, Some("W"));
        assert_eq!(calibration.streak, 2);
    }

    #[test]
    fn test_equal_values_returns_zero() {
        assert_eq!(gini_coefficient(&[1_200.0; 5]), 0.0);
    }

    #[test]
    fn test_single_value_returns_zero() {
        assert_eq!(gini_coefficient(&[1_200.0]), 0.0);
    }

    #[test]
    fn test_empty_returns_zero() {
        assert_eq!(gini_coefficient(&[]), 0.0);
    }

    #[test]
    fn test_unequal_values_positive() {
        assert!(gini_coefficient(&[800.0, 1_000.0, 1_200.0, 1_400.0, 1_600.0]) > 0.0);
    }

    #[test]
    fn test_max_inequality() {
        close(gini_coefficient(&[0.0, 0.0, 0.0, 0.0, 1_000.0]), 0.8, 0.001);
    }

    #[test]
    fn test_symmetric() {
        let left = gini_coefficient(&[800.0, 1_000.0, 1_200.0, 1_400.0, 1_600.0]);
        let right = gini_coefficient(&[1_600.0, 800.0, 1_400.0, 1_000.0, 1_200.0]);
        close(left, right, 1e-10);
    }

    #[test]
    fn test_zero_mean_returns_zero() {
        assert_eq!(gini_coefficient(&[0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn test_negative_mean_returns_zero() {
        assert_eq!(gini_coefficient(&[-10.0, -20.0, -30.0]), 0.0);
    }

    #[test]
    fn test_perfect_positive() {
        close(
            pearson_r(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]).unwrap(),
            1.0,
            1e-10,
        );
    }

    #[test]
    fn test_perfect_negative() {
        close(
            pearson_r(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]).unwrap(),
            -1.0,
            1e-10,
        );
    }

    #[test]
    fn test_too_few_points() {
        assert_eq!(pearson_r(&[1.0, 2.0], &[1.0, 2.0]), None);
    }

    #[test]
    fn test_constant_x_returns_none() {
        assert_eq!(pearson_r(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]), None);
    }

    #[test]
    fn test_constant_y_returns_none() {
        assert_eq!(pearson_r(&[1.0, 2.0, 3.0], &[5.0, 5.0, 5.0]), None);
    }

    #[test]
    fn test_empty_returns_none() {
        assert_eq!(pearson_r(&[], &[]), None);
    }

    fn make_rating_history_entry(
        match_id: i64,
        team_number: i32,
        rating_before: f64,
        expected_win_probability: Option<f64>,
        won: bool,
    ) -> RatingHistoryEntry {
        RatingHistoryEntry {
            match_id: Some(match_id),
            team_number: Some(team_number),
            rating_before: Some(rating_before),
            rating: Some(rating_before + if won { 10.0 } else { -10.0 }),
            rd_before: Some(100.0),
            expected_team_win_prob: expected_win_probability,
            won: Some(won),
            ..RatingHistoryEntry::default()
        }
    }

    fn make_team_entries(
        match_id: i64,
        team_number: i32,
        ratings: &[f64],
        expected_win_probability: Option<f64>,
        won: bool,
    ) -> Vec<RatingHistoryEntry> {
        ratings
            .iter()
            .map(|rating| {
                make_rating_history_entry(
                    match_id,
                    team_number,
                    *rating,
                    expected_win_probability,
                    won,
                )
            })
            .collect()
    }

    #[test]
    fn test_empty_input() {
        let result = compute_team_composition_stats(&[]);
        assert!(result.halves.is_empty());
        assert_eq!(result.total_teams, 0);
        assert_eq!(result.gini_correlation, None);
    }

    #[test]
    fn test_teams_with_fewer_than_5_players_excluded() {
        let entries = vec![
            make_rating_history_entry(1, 1, 1_200.0, Some(0.5), true),
            make_rating_history_entry(1, 1, 1_210.0, Some(0.5), true),
            make_rating_history_entry(1, 1, 1_190.0, Some(0.5), true),
        ];
        assert_eq!(compute_team_composition_stats(&entries).total_teams, 0);
    }

    #[test]
    fn test_below_display_threshold() {
        let mut entries = Vec::new();
        for match_id in 0..4 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[1_200.0, 1_210.0, 1_190.0, 1_205.0, 1_195.0],
                Some(0.5),
                true,
            ));
        }
        let result = compute_team_composition_stats(&entries);
        assert_eq!(result.total_teams, 4);
        assert!(result.halves.is_empty());
    }

    #[test]
    fn test_index_split_with_ties() {
        let mut entries = Vec::new();
        for match_id in 0..6 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[1_200.0; 5],
                Some(0.5),
                match_id < 3,
            ));
        }
        let result = compute_team_composition_stats(&entries);
        assert_eq!(result.total_teams, 6);
        assert_eq!(result.halves.len(), 2);
        assert_eq!(result.halves[0].total, 3);
        assert_eq!(result.halves[1].total, 3);
        assert_eq!(result.halves[0].name, "Similar ratings");
        assert_eq!(result.halves[1].name, "Mixed ratings");
    }

    #[test]
    fn test_overperformance_calculation() {
        let mut entries = Vec::new();
        for match_id in 0..3 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[1_200.0; 5],
                Some(0.4),
                true,
            ));
        }
        for match_id in 3..6 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[800.0, 1_000.0, 1_200.0, 1_400.0, 1_600.0],
                Some(0.6),
                false,
            ));
        }
        let result = compute_team_composition_stats(&entries);
        assert_eq!(result.halves.len(), 2);
        assert_eq!(result.halves[0].name, "Similar ratings");
        close(result.halves[0].overperformance, 0.6, 0.001);
        assert_eq!(result.halves[1].name, "Mixed ratings");
        close(result.halves[1].overperformance, -0.6, 0.001);
    }

    #[test]
    fn test_gini_correlation_computed() {
        let mut entries = Vec::new();
        for match_id in 0..3 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[1_200.0; 5],
                Some(0.5),
                true,
            ));
        }
        for match_id in 3..6 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[800.0, 1_000.0, 1_200.0, 1_400.0, 1_600.0],
                Some(0.5),
                false,
            ));
        }
        assert!(
            compute_team_composition_stats(&entries)
                .gini_correlation
                .is_some()
        );
    }

    #[test]
    fn test_gini_correlation_none_when_constant_gini() {
        let mut entries = Vec::new();
        for match_id in 0..6 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[1_200.0; 5],
                Some(0.5),
                match_id < 3,
            ));
        }
        assert_eq!(
            compute_team_composition_stats(&entries).gini_correlation,
            None
        );
    }

    #[test]
    fn test_mixed_teams_both_sides() {
        let mut entries = Vec::new();
        for match_id in 0..5 {
            entries.extend(make_team_entries(
                match_id,
                1,
                &[1_200.0, 1_210.0, 1_190.0, 1_205.0, 1_195.0],
                Some(0.5),
                true,
            ));
            entries.extend(make_team_entries(
                match_id,
                2,
                &[800.0, 1_000.0, 1_200.0, 1_400.0, 1_600.0],
                Some(0.5),
                false,
            ));
        }
        assert_eq!(compute_team_composition_stats(&entries).total_teams, 10);
    }

    #[test]
    fn test_missing_rating_before_skipped() {
        let mut entries = make_team_entries(
            1,
            1,
            &[1_200.0, 1_210.0, 1_190.0, 1_205.0, 1_195.0],
            Some(0.5),
            true,
        );
        entries[0].rating_before = None;
        assert_eq!(compute_team_composition_stats(&entries).total_teams, 0);
    }

    #[test]
    fn test_missing_expected_win_prob_skipped() {
        let entries = make_team_entries(
            1,
            1,
            &[1_200.0, 1_210.0, 1_190.0, 1_205.0, 1_195.0],
            None,
            true,
        );
        assert_eq!(compute_team_composition_stats(&entries).total_teams, 0);
    }

    fn integration_player(name: &str) -> Player {
        Player {
            mmr: Some(3_000),
            initial_mmr: Some(3_000),
            preferred_roles: Some(vec!["1".to_owned()]),
            main_role: Some("1".to_owned()),
            glicko_rating: Some(1_200.0),
            glicko_rd: Some(100.0),
            glicko_volatility: Some(0.06),
            ..Player::new(name)
        }
    }

    #[test]
    fn test_returns_team_composition_key() {
        let result = compute_calibration_stats(&[integration_player("Player1")], 0, &[], &[]);
        assert_eq!(result.team_composition, TeamCompositionStats::default());
    }

    #[test]
    fn test_team_composition_structure() {
        let result = compute_calibration_stats(&[integration_player("Player1")], 0, &[], &[]);
        assert!(result.team_composition.halves.is_empty());
        assert_eq!(result.team_composition.total_teams, 0);
        assert_eq!(result.team_composition.gini_correlation, None);
    }

    #[test]
    fn test_with_rating_history() {
        let players: Vec<_> = (1..=5)
            .map(|index| integration_player(&format!("Player{index}")))
            .collect();
        let entries = make_team_entries(
            1,
            1,
            &[1_200.0, 1_210.0, 1_190.0, 1_205.0, 1_195.0],
            Some(0.5),
            true,
        );
        let result = compute_calibration_stats(&players, 1, &[], &entries);
        assert_eq!(result.team_composition.total_teams, 1);
    }
}
