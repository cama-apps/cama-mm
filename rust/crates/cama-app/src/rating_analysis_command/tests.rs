use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use cama_domain::openskill::CamaOpenSkillSystem;

use super::*;
use crate::rating_comparison_service::{RatingComparisonMatchData, RatingSystemStats};

#[derive(Default)]
struct StubMatchPort {
    backfill_result: Option<Result<BackfillResult, String>>,
    history: Vec<PlayerOpenSkillHistory>,
    backfill_calls: Vec<(Option<GuildId>, bool)>,
    history_calls: Vec<(DiscordId, Option<GuildId>, usize)>,
}

impl RatingAnalysisMatchPort for StubMatchPort {
    fn backfill_openskill_ratings(
        &mut self,
        guild_id: Option<GuildId>,
        reset_first: bool,
    ) -> Result<BackfillResult, String> {
        self.backfill_calls.push((guild_id, reset_first));
        self.backfill_result
            .clone()
            .unwrap_or_else(|| Ok(BackfillResult::default()))
    }

    fn player_openskill_history(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        limit: usize,
    ) -> Vec<PlayerOpenSkillHistory> {
        self.history_calls.push((discord_id, guild_id, limit));
        self.history.clone()
    }
}

#[derive(Default)]
struct StubPlayerPort {
    players: HashMap<DiscordId, PlayerRatingRecord>,
    ratings: HashMap<DiscordId, (f64, f64)>,
    player_calls: Vec<(DiscordId, Option<GuildId>)>,
    rating_calls: Vec<(DiscordId, Option<GuildId>)>,
}

impl RatingAnalysisPlayerPort for StubPlayerPort {
    fn player(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Option<PlayerRatingRecord> {
        self.player_calls.push((discord_id, guild_id));
        self.players.get(&discord_id).copied()
    }

    fn openskill_rating(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Option<(f64, f64)> {
        self.rating_calls.push((discord_id, guild_id));
        self.ratings.get(&discord_id).copied()
    }
}

#[derive(Default)]
struct StubComparisonPort {
    summary: Option<Result<ComparisonSummaryPayload, String>>,
    curves: Option<Result<CalibrationCurvePayload, String>>,
    summary_calls: Vec<Option<GuildId>>,
    curve_calls: Vec<Option<GuildId>>,
}

impl RatingAnalysisComparisonPort for StubComparisonPort {
    fn comparison_summary(
        &mut self,
        guild_id: Option<GuildId>,
    ) -> Result<ComparisonSummaryPayload, String> {
        self.summary_calls.push(guild_id);
        self.summary.clone().unwrap_or_else(|| {
            Ok(ComparisonSummaryPayload::Error(
                "no comparison configured".to_owned(),
            ))
        })
    }

    fn calibration_curve_data(
        &mut self,
        guild_id: Option<GuildId>,
    ) -> Result<CalibrationCurvePayload, String> {
        self.curve_calls.push(guild_id);
        self.curves.clone().unwrap_or_else(|| {
            Ok(CalibrationCurvePayload::Error(
                "no curves configured".to_owned(),
            ))
        })
    }
}

#[derive(Default)]
struct StubDrawingPort {
    comparison_calls: usize,
    calibration_calls: usize,
    trend_calls: Vec<usize>,
}

impl RatingAnalysisDrawingPort for StubDrawingPort {
    fn rating_comparison_chart(
        &mut self,
        _comparison: &RatingComparisonResult,
    ) -> Result<Vec<u8>, String> {
        self.comparison_calls += 1;
        Ok(fake_png())
    }

    fn calibration_curve_chart(
        &mut self,
        _curves: &CalibrationCurveData,
    ) -> Result<Vec<u8>, String> {
        self.calibration_calls += 1;
        Ok(fake_png())
    }

    fn prediction_over_time_chart(
        &mut self,
        _comparison: &RatingComparisonResult,
        window: usize,
    ) -> Result<Vec<u8>, String> {
        self.trend_calls.push(window);
        Ok(fake_png())
    }
}

fn fake_png() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\nxxxxxxxxxxxxxxxx".to_vec()
}

type Command = RatingAnalysisCommand<
    StubMatchPort,
    StubPlayerPort,
    StubComparisonPort,
    StubDrawingPort,
    CamaOpenSkillSystem,
>;

fn command(
    matches: StubMatchPort,
    players: StubPlayerPort,
    comparisons: Option<StubComparisonPort>,
) -> Command {
    RatingAnalysisCommand::new(
        matches,
        players,
        comparisons,
        StubDrawingPort::default(),
        CamaOpenSkillSystem::new(),
    )
}

fn input(action: &str) -> RatingAnalysisInput {
    RatingAnalysisInput {
        admin_allowed: true,
        action: action.to_owned(),
        guild_id: Some(12_345),
        invoker: MemberTarget::new(1, "Admin"),
        selected_user: None,
    }
}

fn system_stats(name: &str, brier_score: f64, accuracy: f64, log_loss: f64) -> RatingSystemStats {
    RatingSystemStats {
        name: name.to_owned(),
        total_predictions: 100,
        brier_score,
        accuracy,
        calibration_buckets: BTreeMap::new(),
        log_loss,
    }
}

fn match_datum(match_id: i64) -> RatingComparisonMatchData {
    RatingComparisonMatchData {
        match_id,
        match_date: match_id,
        radiant_won: true,
        glicko_radiant_prob: 0.6,
        openskill_radiant_prob: 0.55,
        raw_openskill_radiant_prob: 0.7,
        glicko_correct: true,
        openskill_correct: true,
    }
}

fn summary(
    brier_glicko: f64,
    brier_openskill: f64,
    accuracy_glicko: f64,
    accuracy_openskill: f64,
    match_count: usize,
) -> RatingComparisonResult {
    RatingComparisonResult {
        glicko: system_stats("Glicko-2", brier_glicko, accuracy_glicko, 0.65),
        openskill: system_stats("OpenSkill", brier_openskill, accuracy_openskill, 0.66),
        matches_analyzed: if match_count == 0 { 100 } else { match_count },
        match_data: (0..match_count)
            .map(|match_id| match_datum(match_id as i64))
            .collect(),
    }
}

fn comparison_with(report: RatingComparisonResult) -> StubComparisonPort {
    StubComparisonPort {
        summary: Some(Ok(ComparisonSummaryPayload::Report(report))),
        ..StubComparisonPort::default()
    }
}

fn last_delivery(plan: &CommandPlan) -> &Delivery {
    plan.deliveries.last().expect("command sent a response")
}

fn last_embed(plan: &CommandPlan) -> &EmbedModel {
    last_delivery(plan)
        .embed
        .as_ref()
        .expect("command sent an embed")
}

fn field<'a>(embed: &'a EmbedModel, name: &str) -> &'a str {
    embed
        .fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.value.as_str())
        .expect("embed field exists")
}

#[test]
fn test_non_admin_blocked() {
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(StubComparisonPort::default()),
    );
    let mut request = input("compare");
    request.admin_allowed = false;

    let plan = command.execute(&request);

    assert_eq!(plan.deferred_ephemeral, None);
    assert_eq!(plan.deliveries.len(), 1);
    assert_eq!(last_delivery(&plan).route, DeliveryRoute::InitialResponse);
    assert_eq!(last_delivery(&plan).ephemeral, Some(true));
    assert!(
        last_delivery(&plan)
            .content
            .as_deref()
            .is_some_and(|content| content.contains("admin-only"))
    );
    assert!(command.match_port().backfill_calls.is_empty());
    assert!(
        command
            .comparison_port()
            .expect("configured comparison")
            .summary_calls
            .is_empty()
    );
}

#[test]
fn test_unknown_action_rejected() {
    let mut command = command(StubMatchPort::default(), StubPlayerPort::default(), None);

    let plan = command.execute(&input("bogus"));

    assert_eq!(plan.deferred_ephemeral, None);
    assert_eq!(last_delivery(&plan).route, DeliveryRoute::InitialResponse);
    assert_eq!(last_delivery(&plan).ephemeral, Some(true));
    assert_eq!(
        last_delivery(&plan).content.as_deref(),
        Some("Unknown action: bogus")
    );
}

#[test]
fn test_backfill_success_renders_summary() {
    let matches = StubMatchPort {
        backfill_result: Some(Ok(BackfillResult {
            matches_processed: 100,
            total_matches: Some(100),
            players_updated: 12,
            errors: Vec::new(),
        })),
        ..StubMatchPort::default()
    };
    let mut command = command(matches, StubPlayerPort::default(), None);

    let plan = command.execute(&input("backfill"));

    assert_eq!(command.match_port().backfill_calls, [(Some(12_345), true)]);
    assert_eq!(plan.deliveries.len(), 2);
    let embed = last_embed(&plan);
    assert_eq!(embed.title.as_deref(), Some("OpenSkill Backfill Complete"));
    assert_eq!(field(embed, "Matches Processed"), "100/100");
    assert_eq!(field(embed, "Players Updated"), "12");
}

#[test]
fn test_backfill_includes_truncated_errors() {
    let matches = StubMatchPort {
        backfill_result: Some(Ok(BackfillResult {
            matches_processed: 50,
            total_matches: Some(50),
            players_updated: 3,
            errors: (0..8).map(|index| format!("err{index}")).collect(),
        })),
        ..StubMatchPort::default()
    };
    let mut command = command(matches, StubPlayerPort::default(), None);

    let plan = command.execute(&input("backfill"));

    let errors = field(last_embed(&plan), "Errors");
    assert!(errors.contains("err0"));
    assert!(errors.contains("err4"));
    assert!(!errors.contains("err5"));
    assert!(errors.contains("...and 3 more"));
}

#[test]
fn test_backfill_exception_returns_message() {
    let matches = StubMatchPort {
        backfill_result: Some(Err("DB exploded".to_owned())),
        ..StubMatchPort::default()
    };
    let mut command = command(matches, StubPlayerPort::default(), None);

    let plan = command.execute(&input("backfill"));

    assert_eq!(plan.deliveries.len(), 2);
    assert_eq!(
        last_delivery(&plan).content.as_deref(),
        Some("Backfill failed: DB exploded")
    );
    assert!(last_delivery(&plan).embed.is_none());
}

#[test]
fn test_compare_no_service() {
    let mut command = command(StubMatchPort::default(), StubPlayerPort::default(), None);

    let plan = command.execute(&input("compare"));

    assert!(last_delivery(&plan).embed.is_none());
    assert!(
        last_delivery(&plan)
            .content
            .as_deref()
            .is_some_and(|content| content.contains("not available"))
    );
}

#[test]
fn test_compare_error_payload_handled() {
    let comparisons = StubComparisonPort {
        summary: Some(Ok(ComparisonSummaryPayload::Error(
            "Insufficient data".to_owned(),
        ))),
        ..StubComparisonPort::default()
    };
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(comparisons),
    );

    let plan = command.execute(&input("compare"));

    assert!(last_delivery(&plan).embed.is_none());
    assert!(
        last_delivery(&plan)
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Insufficient data"))
    );
    assert_eq!(
        command.comparison_port().expect("comparison").summary_calls,
        [Some(12_345)]
    );
}

#[test]
fn test_compare_renders_summary_and_chart() {
    let comparisons = comparison_with(summary(0.20, 0.24, 0.65, 0.55, 0));
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(comparisons),
    );

    let plan = command.execute(&input("compare"));

    let delivery = last_delivery(&plan);
    let embed = delivery.embed.as_ref().expect("summary embed");
    assert_eq!(embed.title.as_deref(), Some("Rating System Comparison"));
    assert!(field(embed, "Summary").contains("Glicko-2"));
    assert_eq!(
        delivery
            .attachment
            .as_ref()
            .map(|attachment| attachment.filename.as_str()),
        Some("comparison.png")
    );
    assert_eq!(command.drawing_port().comparison_calls, 1);
}

#[test]
fn test_compare_marginal_difference_summary() {
    let comparisons = comparison_with(summary(0.200, 0.201, 0.55, 0.54, 0));
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(comparisons),
    );

    let plan = command.execute(&input("compare"));

    assert!(field(last_embed(&plan), "Summary").contains("marginal"));
}

#[test]
fn test_calibration_no_service() {
    let mut command = command(StubMatchPort::default(), StubPlayerPort::default(), None);

    let plan = command.execute(&input("calibration"));

    assert!(last_delivery(&plan).embed.is_none());
    assert!(
        last_delivery(&plan)
            .content
            .as_deref()
            .is_some_and(|content| content.contains("not available"))
    );
}

#[test]
fn test_calibration_error_payload() {
    let comparisons = StubComparisonPort {
        curves: Some(Ok(CalibrationCurvePayload::Error(
            "Insufficient data".to_owned(),
        ))),
        ..StubComparisonPort::default()
    };
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(comparisons),
    );

    let plan = command.execute(&input("calibration"));

    assert!(
        last_delivery(&plan)
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Insufficient data"))
    );
}

#[test]
fn test_calibration_renders_chart() {
    let curves = CalibrationCurveData {
        glicko: vec![CalibrationPoint {
            predicted: 0.55,
            actual_rate: 0.6,
            count: 10,
        }],
        openskill: vec![CalibrationPoint {
            predicted: 0.45,
            actual_rate: 0.4,
            count: 10,
        }],
        perfect_line: vec![(0.0, 0.0), (1.0, 1.0)],
    };
    let comparisons = StubComparisonPort {
        curves: Some(Ok(CalibrationCurvePayload::Curves(curves))),
        ..StubComparisonPort::default()
    };
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(comparisons),
    );

    let plan = command.execute(&input("calibration"));

    assert_eq!(
        last_embed(&plan).title.as_deref(),
        Some("Rating System Calibration")
    );
    assert_eq!(
        last_delivery(&plan)
            .attachment
            .as_ref()
            .map(|attachment| attachment.filename.as_str()),
        Some("calibration.png")
    );
    assert_eq!(command.drawing_port().calibration_calls, 1);
}

#[test]
fn test_trend_requires_at_least_20_matches() {
    let comparisons = comparison_with(summary(0.22, 0.23, 0.55, 0.54, 19));
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(comparisons),
    );

    let plan = command.execute(&input("trend"));

    assert!(last_delivery(&plan).embed.is_none());
    assert!(
        last_delivery(&plan)
            .content
            .as_deref()
            .is_some_and(|content| content.contains("20 matches"))
    );
    assert!(command.drawing_port().trend_calls.is_empty());
}

#[test]
fn test_trend_renders_chart_with_enough_matches() {
    let comparisons = comparison_with(summary(0.22, 0.23, 0.55, 0.54, 25));
    let mut command = command(
        StubMatchPort::default(),
        StubPlayerPort::default(),
        Some(comparisons),
    );

    let plan = command.execute(&input("trend"));

    assert_eq!(
        last_embed(&plan).title.as_deref(),
        Some("Prediction Accuracy Over Time")
    );
    assert_eq!(
        last_delivery(&plan)
            .attachment
            .as_ref()
            .map(|attachment| attachment.filename.as_str()),
        Some("trend.png")
    );
    assert_eq!(command.drawing_port().trend_calls, [20]);
}

#[test]
fn test_unregistered_target() {
    let mut command = command(StubMatchPort::default(), StubPlayerPort::default(), None);
    let mut request = input("player");
    request.selected_user = Some(MemberTarget::new(123, "Ghost"));

    let plan = command.execute(&request);

    assert_eq!(
        last_delivery(&plan).content.as_deref(),
        Some("Ghost is not registered.")
    );
    assert!(command.player_port().rating_calls.is_empty());
}

#[test]
fn test_player_with_openskill_data_calibrated() {
    let matches = StubMatchPort {
        history: vec![PlayerOpenSkillHistory {
            os_mu_before: 25.0,
            os_mu_after: 26.5,
            os_sigma_before: 5.0,
            os_sigma_after: 4.5,
            won: Some(true),
            fantasy_weight: Some(1.5),
            streak_length: Some(3),
            streak_multiplier: Some(1.2),
        }],
        ..StubMatchPort::default()
    };
    let mut players = StubPlayerPort::default();
    players.players.insert(
        42,
        PlayerRatingRecord {
            glicko_rating: Some(1_500.0),
            glicko_rd: Some(80.0),
        },
    );
    players.ratings.insert(42, (30.0, 3.0));
    let mut command = command(matches, players, None);
    let mut request = input("player");
    request.selected_user = Some(MemberTarget::new(42, "Star"));

    let plan = command.execute(&request);

    let embed = last_embed(&plan);
    assert!(
        embed
            .title
            .as_deref()
            .is_some_and(|title| title.contains("Star"))
    );
    assert!(field(embed, "Normalized Rating").contains("250"));
    assert!(field(embed, "Glicko-2 Rating").contains("1500"));
    assert!(field(embed, "Calibrated").contains("Yes"));
    assert!(field(embed, "Skill (μ)").contains("30.00"));
    assert!(field(embed, "Uncertainty (σ)").contains("3.000"));
    assert!(field(embed, "Ordinal (μ-3σ)").contains("21.00"));
    let history = field(embed, "Recent OpenSkill Changes");
    assert!(history.contains("+75"));
    assert!(!history.contains("+1.50"));
    assert!(history.contains("(perf×1.500)"));
    assert!(history.contains("(streak W3×1.20)"));
    assert_eq!(command.match_port().history_calls, [(42, Some(12_345), 5)]);
}

struct SpyMath {
    system: CamaOpenSkillSystem,
    calibration_calls: RefCell<Vec<f64>>,
}

impl RatingAnalysisMath for SpyMath {
    fn ordinal(&self, mu: f64, sigma: f64) -> f64 {
        self.system.ordinal(mu, sigma, 3.0)
    }

    fn mu_to_display(&self, mu: f64) -> i32 {
        self.system.mu_to_display(mu)
    }

    fn is_calibrated(&self, sigma: f64) -> bool {
        self.calibration_calls.borrow_mut().push(sigma);
        true
    }

    fn calibration_threshold(&self) -> f64 {
        self.system.config().calibration_threshold
    }
}

#[test]
fn test_player_uses_canonical_calibration_helper() {
    let mut players = StubPlayerPort::default();
    players.players.insert(9, PlayerRatingRecord::default());
    players.ratings.insert(9, (20.0, 6.0));
    let math = SpyMath {
        system: CamaOpenSkillSystem::new(),
        calibration_calls: RefCell::new(Vec::new()),
    };
    let mut command = RatingAnalysisCommand::new(
        StubMatchPort::default(),
        players,
        None::<StubComparisonPort>,
        StubDrawingPort::default(),
        math,
    );
    let mut request = input("player");
    request.selected_user = Some(MemberTarget::new(9, "Canonical"));

    let plan = command.execute(&request);

    assert!(field(last_embed(&plan), "Calibrated").contains("Yes"));
    assert_eq!(
        command.math_port().calibration_calls.borrow().as_slice(),
        &[6.0]
    );
}

#[test]
fn test_player_with_high_sigma_not_calibrated() {
    let mut players = StubPlayerPort::default();
    players.players.insert(
        9,
        PlayerRatingRecord {
            glicko_rating: Some(1_400.0),
            glicko_rd: Some(200.0),
        },
    );
    players.ratings.insert(9, (20.0, 6.0));
    let mut command = command(StubMatchPort::default(), players, None);
    let mut request = input("player");
    request.selected_user = Some(MemberTarget::new(9, "Newbie"));

    let plan = command.execute(&request);

    let calibrated = field(last_embed(&plan), "Calibrated");
    assert!(calibrated.contains("No"));
    assert!(calibrated.contains("4.0"));
}

#[test]
fn test_player_without_openskill_shows_help_message() {
    let mut players = StubPlayerPort::default();
    players.players.insert(5, PlayerRatingRecord::default());
    let mut command = command(StubMatchPort::default(), players, None);
    let mut request = input("player");
    request.selected_user = Some(MemberTarget::new(5, "Empty"));

    let plan = command.execute(&request);

    let description = last_embed(&plan)
        .description
        .as_deref()
        .expect("help description");
    assert!(description.contains("OpenSkill"));
    assert!(description.to_lowercase().contains("backfill"));
}

#[test]
fn test_player_defaults_to_invoker() {
    let mut players = StubPlayerPort::default();
    players.players.insert(
        1,
        PlayerRatingRecord {
            glicko_rating: Some(1_500.0),
            glicko_rd: Some(80.0),
        },
    );
    players.ratings.insert(1, (25.0, 3.5));
    let mut command = command(StubMatchPort::default(), players, None);

    let plan = command.execute(&input("player"));

    assert!(
        last_embed(&plan)
            .title
            .as_deref()
            .is_some_and(|title| title.contains("Admin"))
    );
    assert_eq!(command.player_port().player_calls, [(1, Some(12_345))]);
}
