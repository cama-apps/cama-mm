use std::collections::HashMap;

use super::*;

#[derive(Clone, Debug, Default)]
struct FakeMatches {
    matches: Vec<HistoricalRatingMatch>,
    ratings_by_match: HashMap<MatchId, HistoricalOpenSkillRatings>,
    all_calls: Vec<Option<GuildId>>,
    os_calls: Vec<(Vec<MatchId>, Option<GuildId>)>,
}

impl FakeMatches {
    fn new(matches: Vec<HistoricalRatingMatch>) -> Self {
        Self {
            matches,
            ..Self::default()
        }
    }

    fn with_ratings(
        matches: Vec<HistoricalRatingMatch>,
        ratings_by_match: HashMap<MatchId, HistoricalOpenSkillRatings>,
    ) -> Self {
        Self {
            matches,
            ratings_by_match,
            ..Self::default()
        }
    }
}

impl RatingComparisonMatchPort for FakeMatches {
    fn all_matches_with_predictions(
        &mut self,
        guild_id: Option<GuildId>,
    ) -> Vec<HistoricalRatingMatch> {
        self.all_calls.push(guild_id);
        self.matches.clone()
    }

    fn openskill_ratings_for_matches(
        &mut self,
        match_ids: &[MatchId],
        guild_id: Option<GuildId>,
    ) -> HashMap<MatchId, HistoricalOpenSkillRatings> {
        self.os_calls.push((match_ids.to_vec(), guild_id));
        match_ids
            .iter()
            .map(|match_id| {
                (
                    *match_id,
                    self.ratings_by_match
                        .get(match_id)
                        .cloned()
                        .unwrap_or_else(default_ratings),
                )
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
struct FakeCurrentPredictions {
    calls: Vec<(Vec<PlayerId>, Vec<PlayerId>, Option<GuildId>)>,
}

impl CurrentOpenSkillPredictionPort for FakeCurrentPredictions {
    fn predict_from_current_ratings(
        &mut self,
        team1_players: &[PlayerId],
        team2_players: &[PlayerId],
        guild_id: Option<GuildId>,
    ) -> f64 {
        self.calls
            .push((team1_players.to_vec(), team2_players.to_vec(), guild_id));
        0.5
    }
}

type Service = RatingComparisonService<FakeMatches, FakeCurrentPredictions>;

fn service(matches: Vec<HistoricalRatingMatch>) -> Service {
    Service::new(FakeMatches::new(matches), FakeCurrentPredictions::default())
}

fn service_with_ratings(
    matches: Vec<HistoricalRatingMatch>,
    ratings_by_match: HashMap<MatchId, HistoricalOpenSkillRatings>,
) -> Service {
    Service::new(
        FakeMatches::with_ratings(matches, ratings_by_match),
        FakeCurrentPredictions::default(),
    )
}

fn default_ratings() -> HistoricalOpenSkillRatings {
    HistoricalOpenSkillRatings {
        team1: vec![Rating::new(30.0, 8.0); 5],
        team2: vec![Rating::new(30.0, 8.0); 5],
    }
}

fn historical_match(match_id: MatchId) -> HistoricalRatingMatch {
    HistoricalRatingMatch {
        match_id,
        match_date: 0,
        winning_team: if match_id % 2 == 0 { 1 } else { 2 },
        expected_radiant_win_prob: Some(0.5),
        team1_players: vec![1, 2, 3, 4, 5],
        team2_players: vec![6, 7, 8, 9, 10],
        ..HistoricalRatingMatch::default()
    }
}

fn datum(probability: f64, radiant_won: bool) -> RatingComparisonMatchData {
    RatingComparisonMatchData {
        match_id: 0,
        match_date: 0,
        radiant_won,
        glicko_radiant_prob: probability,
        openskill_radiant_prob: probability,
        raw_openskill_radiant_prob: probability,
        glicko_correct: (probability >= 0.5) == radiant_won,
        openskill_correct: (probability >= 0.5) == radiant_won,
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn test_calculate_system_stats_brier_and_accuracy_hand_computed() {
    let data = [
        datum(0.8, true),
        datum(0.6, false),
        datum(0.3, false),
        datum(0.5, true),
    ];
    let stats = Service::calculate_system_stats("Glicko-2", &data, PredictionSystem::Glicko);

    assert_eq!(stats.total_predictions, 4);
    close(stats.brier_score, 0.185);
    close(stats.accuracy, 0.75);
}

#[test]
fn test_calculate_system_stats_perfect_predictions_score_zero_brier() {
    let data = [datum(1.0, true), datum(0.0, false), datum(1.0, true)];
    let stats = Service::calculate_system_stats("Glicko-2", &data, PredictionSystem::Glicko);

    close(stats.brier_score, 0.0);
    close(stats.accuracy, 1.0);
}

#[test]
fn test_calculate_system_stats_accuracy_counts_underdog_wins_correctly() {
    let data = [datum(0.5, false), datum(0.49, false)];
    let stats = Service::calculate_system_stats("Glicko-2", &data, PredictionSystem::Glicko);

    close(stats.accuracy, 0.5);
}

#[test]
fn test_calculate_system_stats_empty_returns_coinflip_defaults() {
    let stats = Service::calculate_system_stats("Glicko-2", &[], PredictionSystem::Glicko);

    assert_eq!(stats.total_predictions, 0);
    close(stats.brier_score, 0.25);
    close(stats.accuracy, 0.5);
    close(stats.log_loss, 1.0);
    assert!(stats.calibration_buckets.is_empty());
}

#[test]
fn test_calculate_system_stats_calibration_bucket_rates() {
    let data = [datum(0.82, true), datum(0.88, false), datum(0.35, false)];
    let stats = Service::calculate_system_stats("Glicko-2", &data, PredictionSystem::Glicko);

    let high = &stats.calibration_buckets["80-90%"];
    assert_eq!(high.count, 2);
    assert_eq!(high.actual_wins, 1);
    close(high.avg_predicted, 0.85);
    close(high.actual_rate, 0.5);

    let low = &stats.calibration_buckets["30-40%"];
    assert_eq!(low.count, 1);
    close(low.actual_rate, 0.0);

    let empty = &stats.calibration_buckets["50-60%"];
    assert_eq!(empty.count, 0);
    close(empty.avg_predicted, 0.0);
    close(empty.actual_rate, 0.0);
}

#[test]
fn test_get_bucket_key_boundaries_0_0_0_10_percent() {
    assert_eq!(Service::bucket_key(0.0), "0-10%");
}

#[test]
fn test_get_bucket_key_boundaries_0_05_0_10_percent() {
    assert_eq!(Service::bucket_key(0.05), "0-10%");
}

#[test]
fn test_get_bucket_key_boundaries_0_099_0_10_percent() {
    assert_eq!(Service::bucket_key(0.099), "0-10%");
}

#[test]
fn test_get_bucket_key_boundaries_0_1_10_20_percent() {
    assert_eq!(Service::bucket_key(0.1), "10-20%");
}

#[test]
fn test_get_bucket_key_boundaries_0_25_20_30_percent() {
    assert_eq!(Service::bucket_key(0.25), "20-30%");
}

#[test]
fn test_get_bucket_key_boundaries_0_4_40_50_percent() {
    assert_eq!(Service::bucket_key(0.4), "40-50%");
}

#[test]
fn test_get_bucket_key_boundaries_0_49_40_50_percent() {
    assert_eq!(Service::bucket_key(0.49), "40-50%");
}

#[test]
fn test_get_bucket_key_boundaries_0_5_50_60_percent() {
    assert_eq!(Service::bucket_key(0.5), "50-60%");
}

#[test]
fn test_get_bucket_key_boundaries_0_7_70_80_percent() {
    assert_eq!(Service::bucket_key(0.7), "70-80%");
}

#[test]
fn test_get_bucket_key_boundaries_0_89_80_90_percent() {
    assert_eq!(Service::bucket_key(0.89), "80-90%");
}

#[test]
fn test_get_bucket_key_boundaries_0_9_90_100_percent() {
    assert_eq!(Service::bucket_key(0.9), "90-100%");
}

#[test]
fn test_get_bucket_key_boundaries_1_0_90_100_percent() {
    assert_eq!(Service::bucket_key(1.0), "90-100%");
}

#[test]
fn test_analyze_returns_none_when_no_matches() {
    let mut comparison = service(Vec::new());

    assert!(comparison.analyze_rating_systems(Some(1)).is_none());
    assert_eq!(comparison.match_port().all_calls, [Some(1)]);
    assert!(comparison.match_port().os_calls.is_empty());
}

#[test]
fn test_analyze_returns_none_below_ten_complete_matches() {
    let matches = (0..9).map(historical_match).collect();
    let mut comparison = service(matches);

    assert!(comparison.analyze_rating_systems(Some(1)).is_none());
    assert_eq!(
        comparison.match_port().os_calls,
        [(Vec::from_iter(0..9), Some(1))]
    );
}

#[test]
fn test_analyze_skips_matches_missing_prob_or_rosters_then_guards() {
    let mut matches: Vec<_> = (0..2).map(historical_match).collect();
    matches.extend((0..5).map(|index| HistoricalRatingMatch {
        expected_radiant_win_prob: None,
        ..historical_match(100 + index)
    }));
    matches.extend((0..5).map(|index| HistoricalRatingMatch {
        team1_players: Vec::new(),
        team2_players: Vec::new(),
        ..historical_match(200 + index)
    }));
    let mut comparison = service(matches);

    assert!(comparison.analyze_rating_systems(Some(1)).is_none());
    assert_eq!(comparison.match_port().os_calls, [(vec![0, 1], Some(1))]);
}

#[test]
fn test_analyze_passes_guild_id_to_historical_openskill_snapshots() {
    let guild_id = 12_345;
    let matches = (0..10).map(historical_match).collect();
    let mut comparison = service(matches);

    let result = comparison.analyze_rating_systems(Some(guild_id));

    assert!(result.is_some());
    assert_eq!(
        comparison.match_port().os_calls,
        [(Vec::from_iter(0..10), Some(guild_id))]
    );
    assert!(comparison.current_prediction_port().calls.is_empty());
}

#[test]
fn test_analyze_uses_historical_openskill_not_current_rating_lookup() {
    let matches: Vec<_> = (0..10).map(historical_match).collect();
    let ratings_by_match = (0..10)
        .map(|match_id| {
            (
                match_id,
                HistoricalOpenSkillRatings {
                    team1: vec![Rating::new(60.0, 4.0); 5],
                    team2: vec![Rating::new(35.0, 4.0); 5],
                },
            )
        })
        .collect();
    let mut comparison = service_with_ratings(matches, ratings_by_match);

    let result = comparison
        .analyze_rating_systems(Some(1))
        .expect("ten complete historical matches produce a result");

    assert!(comparison.current_prediction_port().calls.is_empty());
    assert!(
        result
            .match_data
            .iter()
            .all(|datum| datum.openskill_radiant_prob > 0.5)
    );
    assert!(
        result
            .match_data
            .iter()
            .all(|datum| { datum.raw_openskill_radiant_prob > datum.openskill_radiant_prob })
    );
}
