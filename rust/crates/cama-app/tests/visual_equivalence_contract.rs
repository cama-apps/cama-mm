use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use cama_app::blame_luke_media::{
    BLAME_LUKE_FINAL_HOLD_MS, BLAME_LUKE_FRAME_COUNT, BLAME_LUKE_HEIGHT, BLAME_LUKE_REASONS,
    BLAME_LUKE_WIDTH, render_blame_luke,
};
use cama_app::dig_assets::{DigRenderPort, MediaFormat, RenderRequest, inspect_media};
use cama_app::dig_media_runtime::NativeDigRenderer;
use cama_app::drawing::{
    AdvantageData, BalancePoint, RatingHistoryEntry, draw_advantage_graph, draw_balance_chart,
    draw_prediction_market_chart, draw_rating_history_chart,
};
use cama_app::herogrid::draw_hero_grid;
use cama_app::neon_degen::GifAsset;
use cama_app::pet_assets::{
    FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest, decode_png_raster,
    inspect_png, render_pet_card,
};
use cama_app::post_match_gif_media::render_post_match_gif;
use cama_app::rating_analysis_command::{
    CalibrationCurveData, CalibrationPoint, RatingAnalysisDrawingPort,
};
use cama_app::rating_analysis_media::NativeRatingAnalysisDrawing;
use cama_app::rating_comparison_service::{
    RatingComparisonMatchData, RatingComparisonResult, RatingSystemStats,
};
use cama_app::scout::media::NativeScoutImageRenderer;
use cama_app::scout::{ScoutData, ScoutHero, ScoutImageRenderer, ScoutReportInput};
use cama_db::herogrid_repository::{HeroGridPlayer, HeroGridStat};
use cama_domain::pet::{PetMood, PetStage};
use gif::{ColorOutput, DecodeOptions};
use serde::Deserialize;

const FIXTURE_JSON: &str = include_str!("../../../../scripts/visual_equivalence_fixture.json");
const FOREGROUND_THRESHOLD: u8 = 80;

#[derive(Debug, Deserialize)]
struct Fixture {
    chart: ChartFixture,
    animation: AnimationFixture,
    terminal_crash: TerminalCrashFixture,
    pinnacle: PinnacleFixture,
    balance: BalanceFixture,
    rating_history: RatingHistoryFixture,
    rating_analysis: RatingAnalysisFixture,
    advantage: AdvantageFixture,
    pet: PetFixture,
    blame_luke: BlameLukeFixture,
    scout: ScoutFixture,
    hero_grid: HeroGridFixture,
}

#[derive(Debug, Deserialize)]
struct BalanceFixture {
    username: String,
    series: Vec<(i32, i64, String)>,
    source_totals: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Deserialize)]
struct RatingHistoryFixture {
    username: String,
    entries: Vec<RatingHistoryEntryFixture>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct RatingHistoryEntryFixture {
    rating: Option<f64>,
    os_mu_after: Option<f64>,
    won: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RatingAnalysisFixture {
    comparison: RatingComparisonFixture,
    calibration: CalibrationFixture,
    trend: TrendFixture,
}

#[derive(Debug, Deserialize)]
struct CalibrationFixture {
    glicko: Vec<[f64; 3]>,
    openskill: Vec<[f64; 3]>,
}

#[derive(Debug, Deserialize)]
struct TrendFixture {
    window: usize,
    match_data: Vec<TrendMatchFixture>,
}

#[derive(Debug, Deserialize)]
struct TrendMatchFixture {
    glicko_correct: bool,
    openskill_correct: bool,
}

#[derive(Debug, Deserialize)]
struct RatingComparisonFixture {
    matches_analyzed: usize,
    glicko: RatingComparisonStatsFixture,
    openskill: RatingComparisonStatsFixture,
}

#[derive(Debug, Deserialize)]
struct RatingComparisonStatsFixture {
    brier_score: f64,
    accuracy: f64,
    log_loss: f64,
}

#[derive(Debug, Deserialize)]
struct AdvantageFixture {
    match_id: i64,
    radiant_gold_adv: Vec<f64>,
    radiant_xp_adv: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct PetFixture {
    species_id: String,
    stage: String,
    mood: String,
    seed: i64,
    accessory: Option<String>,
    components_path: String,
    attachment_filename: String,
    embed_image: String,
}

#[derive(Debug, Deserialize)]
struct BlameLukeFixture {
    selected_index: usize,
}

#[derive(Debug, Deserialize)]
struct ScoutFixture {
    player_count: usize,
    total_matches: i64,
    player_names: Vec<String>,
    title: String,
    heroes: Vec<ScoutHeroFixture>,
    portrait_mode: String,
}

#[derive(Debug, Deserialize)]
struct ScoutHeroFixture {
    hero_id: i64,
    games: i64,
    wins: i64,
    losses: i64,
    bans: i64,
    primary_role: i64,
}

#[derive(Debug, Deserialize)]
struct HeroGridFixture {
    title: String,
    min_games: i64,
    players: Vec<HeroGridPlayerFixture>,
    stats: Vec<HeroGridStatFixture>,
}

#[derive(Debug, Deserialize)]
struct HeroGridPlayerFixture {
    discord_id: i64,
    name: String,
}

#[derive(Debug, Deserialize)]
struct HeroGridStatFixture {
    discord_id: i64,
    hero_id: i64,
    games: i64,
    wins: i64,
}

#[derive(Debug, Deserialize)]
struct ChartFixture {
    market_id: i64,
    title: Option<String>,
    snapshots: Vec<(i64, i64)>,
    created_at: i64,
    now: i64,
}

#[derive(Debug, Deserialize)]
struct AnimationFixture {
    name: String,
    value: i64,
    theme: String,
}

#[derive(Debug, Deserialize)]
struct TerminalCrashFixture {
    name: String,
    filing_number: u32,
}

#[derive(Debug, Deserialize)]
struct PinnacleFixture {
    source_path: String,
    boss_id: String,
    secret: bool,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE_JSON).expect("visual fixture is typed Rust input")
}

fn fixture_source_path(source_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("scripts")
        .join(source_path)
}

fn rating_stats(
    name: &str,
    total_predictions: usize,
    stats: &RatingComparisonStatsFixture,
) -> RatingSystemStats {
    RatingSystemStats {
        name: name.to_owned(),
        total_predictions,
        brier_score: stats.brier_score,
        accuracy: stats.accuracy,
        calibration_buckets: std::collections::BTreeMap::new(),
        log_loss: stats.log_loss,
    }
}

fn pixel_metrics(left: &[u8], right: &[u8]) -> (f64, f64, f64) {
    assert_eq!(left.len(), right.len());
    let mut absolute = 0_u64;
    let mut squared = 0_u64;
    let mut equal = 0_u64;
    for (&first, &second) in left.iter().zip(right) {
        let difference = first.abs_diff(second);
        absolute += u64::from(difference);
        squared += u64::from(difference) * u64::from(difference);
        equal += u64::from(first == second);
    }
    let length = left.len() as f64;
    (
        absolute as f64 / (255.0 * length),
        (squared as f64 / length).sqrt() / 255.0,
        equal as f64 / length,
    )
}

fn foreground_signature(
    rgba: &[u8],
    width: usize,
    height: usize,
) -> (usize, BTreeSet<(usize, usize)>) {
    assert_eq!(rgba.len(), width * height * 4);
    let mut interior = 0;
    let mut occupied = BTreeSet::new();
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        if pixel[..3].iter().copied().max().unwrap_or(0) <= FOREGROUND_THRESHOLD {
            continue;
        }
        let x = index % width;
        let y = index / width;
        occupied.insert((x * 10 / width, y * 10 / height));
        if (24..width - 24).contains(&x) && (24..height - 24).contains(&y) {
            interior += 1;
        }
    }
    (interior, occupied)
}

fn foreground_gate(
    reference: &[u8],
    candidate: &[u8],
    width: usize,
    height: usize,
    minimum_count_ratio: f64,
    minimum_grid_iou: f64,
) -> bool {
    let (reference_count, reference_cells) = foreground_signature(reference, width, height);
    let (candidate_count, candidate_cells) = foreground_signature(candidate, width, height);
    if reference_count == 0
        || (candidate_count as f64) < 200.0_f64.max(reference_count as f64 * minimum_count_ratio)
        || reference_cells.is_empty()
    {
        return false;
    }
    let intersection = reference_cells.intersection(&candidate_cells).count();
    let union = reference_cells.union(&candidate_cells).count();
    union > 0 && intersection as f64 / union as f64 >= minimum_grid_iou
}

#[test]
fn visual_fixture_has_typed_chart_and_animation_inputs() {
    let fixture = fixture();
    assert_eq!(fixture.chart.market_id, 42);
    assert_eq!(fixture.chart.snapshots.len(), 4);
    assert!(fixture.chart.created_at < fixture.chart.now);
    assert_eq!(fixture.animation.name, "Client 47");
    assert_eq!(fixture.animation.value, 1_337);
    assert_eq!(fixture.animation.theme, "odds_anomaly");
    assert_eq!(fixture.terminal_crash.name, "Client 47");
    assert_eq!(fixture.terminal_crash.filing_number, 5);
    assert_eq!(
        fixture.pinnacle.source_path,
        "../assets/dig/bosses/lantern_engine_encounter.png"
    );
    assert_eq!(fixture.pinnacle.boss_id, "lantern_engine");
    assert!(fixture.pinnacle.secret);
    assert_eq!(fixture.balance.username, "Visual Balance");
    assert_eq!(fixture.balance.series.len(), 7);
    assert_eq!(fixture.balance.source_totals.len(), 7);
    assert_eq!(fixture.rating_history.username, "Client 47");
    assert_eq!(fixture.rating_history.entries.len(), 6);
    assert!(
        fixture
            .rating_history
            .entries
            .iter()
            .any(|entry| entry.rating.is_none())
    );
    assert!(
        fixture
            .rating_history
            .entries
            .iter()
            .any(|entry| entry.os_mu_after.is_none())
    );
    assert_eq!(fixture.rating_analysis.comparison.matches_analyzed, 25);
    assert_eq!(fixture.rating_analysis.comparison.glicko.brier_score, 0.21);
    assert_eq!(fixture.rating_analysis.comparison.openskill.accuracy, 0.72);
    assert_eq!(fixture.rating_analysis.calibration.glicko.len(), 5);
    assert_eq!(fixture.rating_analysis.calibration.openskill.len(), 5);
    assert_eq!(fixture.rating_analysis.trend.window, 20);
    assert_eq!(fixture.rating_analysis.trend.match_data.len(), 28);
    assert_eq!(fixture.advantage.match_id, 4_242);
    assert_eq!(fixture.advantage.radiant_gold_adv.len(), 7);
    assert_eq!(fixture.advantage.radiant_xp_adv.len(), 7);
    assert_eq!(fixture.pet.species_id, "common_cama");
    assert_eq!(fixture.pet.stage, "adult");
    assert_eq!(fixture.pet.mood, "happy");
    assert_eq!(fixture.pet.seed, 7);
    assert_eq!(fixture.pet.accessory.as_deref(), Some("red_bow"));
    assert_eq!(fixture.pet.components_path, "../assets/pets/components");
    assert_eq!(
        fixture.pet.attachment_filename,
        "pet_common_cama_adult_happy.png"
    );
    assert_eq!(
        fixture.pet.embed_image,
        "attachment://pet_common_cama_adult_happy.png"
    );
    assert_eq!(fixture.blame_luke.selected_index, 4);
    assert!(fixture.blame_luke.selected_index < BLAME_LUKE_REASONS.len());
    assert_eq!(fixture.scout.player_count, 3);
    assert_eq!(fixture.scout.total_matches, 24);
    assert_eq!(
        fixture.scout.player_names,
        vec!["Ada".to_owned(), "Linus".to_owned(), "Grace".to_owned()]
    );
    assert_eq!(fixture.scout.title, "SCOUT: Radiant");
    assert_eq!(fixture.scout.heroes.len(), 3);
    assert_eq!(fixture.scout.heroes[0].hero_id, 1);
    assert_eq!(fixture.scout.portrait_mode, "cache_miss_fallback");
    assert_eq!(fixture.hero_grid.title, "Hero Grid: Visual Fixture");
    assert_eq!(fixture.hero_grid.min_games, 2);
    assert_eq!(fixture.hero_grid.players.len(), 4);
    assert_eq!(fixture.hero_grid.stats.len(), 18);
    assert_eq!(fixture.hero_grid.players[0].discord_id, 101);
    assert_eq!(fixture.hero_grid.stats[0].hero_id, 1);
}

#[test]
fn native_hero_grid_fixture_render_is_deterministic_and_preserves_grid_geometry() {
    let fixture = fixture();
    let stats = fixture
        .hero_grid
        .stats
        .iter()
        .map(|stat| HeroGridStat {
            discord_id: stat.discord_id,
            hero_id: stat.hero_id,
            games: stat.games,
            wins: stat.wins,
        })
        .collect::<Vec<_>>();
    let players = fixture
        .hero_grid
        .players
        .iter()
        .map(|player| HeroGridPlayer {
            discord_id: player.discord_id,
            name: player.name.clone(),
        })
        .collect::<Vec<_>>();
    let first = draw_hero_grid(
        &stats,
        &players,
        fixture.hero_grid.min_games,
        &fixture.hero_grid.title,
    )
    .expect("render Hero Grid fixture")
    .into_inner();
    let second = draw_hero_grid(
        &stats,
        &players,
        fixture.hero_grid.min_games,
        &fixture.hero_grid.title,
    )
    .expect("render Hero Grid fixture again")
    .into_inner();
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("Hero Grid fixture must be a PNG");
    assert_eq!((raster.width, raster.height), (370, 406));
    for expected_color in [
        [87, 242, 135, 255],
        [124, 179, 66, 255],
        [254, 231, 92, 255],
        [237, 66, 69, 255],
    ] {
        assert!(
            raster
                .pixels
                .chunks_exact(4)
                .any(|pixel| pixel == expected_color),
            "Hero Grid fixture should include every win-rate color bracket"
        );
    }
    assert!(
        raster
            .pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3].iter().copied().max().unwrap_or(0) > FOREGROUND_THRESHOLD)
            .count()
            > 1_000,
        "Hero Grid fixture must contain visible rows, labels, and circles"
    );
}

#[test]
fn native_scout_fixture_render_is_deterministic_and_preserves_report_geometry() {
    let fixture = fixture();
    assert_eq!(fixture.scout.portrait_mode, "cache_miss_fallback");
    let report = ScoutReportInput {
        scout_data: ScoutData {
            player_count: fixture.scout.player_count,
            total_matches: Some(fixture.scout.total_matches),
            heroes: fixture
                .scout
                .heroes
                .iter()
                .map(|hero| ScoutHero {
                    hero_id: hero.hero_id,
                    games: hero.games,
                    wins: hero.wins,
                    losses: hero.losses,
                    bans: hero.bans,
                    primary_role: hero.primary_role,
                })
                .collect(),
        },
        player_names: fixture.scout.player_names.clone(),
        title: fixture.scout.title.clone(),
    };
    let renderer = NativeScoutImageRenderer::default();
    let first = renderer.render(&report).expect("render scout fixture");
    let second = renderer
        .render(&report)
        .expect("render scout fixture again");
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("scout fixture must be a PNG");
    assert_eq!((raster.width, raster.height), (360, 12 + 50 + 3 * 32 + 12));
    assert!(
        raster
            .pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3].iter().copied().max().unwrap_or(0) > FOREGROUND_THRESHOLD)
            .count()
            > 200,
        "scout fixture must contain visible report structure"
    );
}

#[test]
fn native_blame_luke_fixture_render_has_exact_playback_contract() {
    let fixture = fixture();
    let first = render_blame_luke(fixture.blame_luke.selected_index)
        .expect("render fixture Blame Luke GIF");
    let second = render_blame_luke(fixture.blame_luke.selected_index)
        .expect("render fixture Blame Luke GIF again");
    assert_eq!(first.bytes, second.bytes);
    assert_eq!(first.selected_index, fixture.blame_luke.selected_index);
    assert_eq!(first.frame_durations_ms.len(), BLAME_LUKE_FRAME_COUNT);
    assert_eq!(
        first.frame_durations_ms.last(),
        Some(&BLAME_LUKE_FINAL_HOLD_MS)
    );

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(first.bytes))
        .expect("decode fixture Blame Luke GIF");
    assert_eq!(
        (decoder.width(), decoder.height()),
        (BLAME_LUKE_WIDTH, BLAME_LUKE_HEIGHT)
    );
    let mut durations = Vec::new();
    let mut frame_count = 0;
    while let Some(frame) = decoder
        .read_next_frame()
        .expect("read fixture Blame Luke frame")
    {
        durations.push(u32::from(frame.delay) * 10);
        frame_count += 1;
    }
    assert_eq!(frame_count, BLAME_LUKE_FRAME_COUNT);
    assert_eq!(durations, first.frame_durations_ms);
    assert_eq!(durations.last(), Some(&BLAME_LUKE_FINAL_HOLD_MS));
}

#[test]
fn native_advantage_fixture_render_is_deterministic_and_semantically_layered() {
    let fixture = fixture();
    let data = AdvantageData {
        radiant_gold: fixture.advantage.radiant_gold_adv.clone(),
        radiant_xp: fixture.advantage.radiant_xp_adv.clone(),
    };
    let first = draw_advantage_graph(&data, Some(fixture.advantage.match_id))
        .expect("advantage fixture should render")
        .into_inner();
    let second = draw_advantage_graph(&data, Some(fixture.advantage.match_id))
        .expect("advantage fixture should render twice")
        .into_inner();
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("decode advantage PNG");
    assert_eq!((raster.width, raster.height), (790, 340));
    assert!(
        raster
            .pixels
            .chunks_exact(4)
            .any(|pixel| pixel == [254, 231, 92, 255]),
        "gold series should be visible"
    );
    assert!(
        raster
            .pixels
            .chunks_exact(4)
            .any(|pixel| pixel == [88, 101, 242, 255]),
        "XP series should be visible"
    );
    assert!(
        raster.pixels.chunks_exact(4).any(|pixel| {
            u16::from(pixel[1]) > u16::from(pixel[0]) + 10
                && u16::from(pixel[1]) > u16::from(pixel[2]) + 5
        }),
        "positive advantage fill should be visible"
    );
}

#[test]
fn native_pet_fixture_render_uses_production_hybrid_and_attachment_contract() {
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/pets/components");
    let pets = components.parent().expect("components parent");
    let request = PetRenderRequest {
        species_id: "common_cama",
        stage: PetStage::Adult,
        mood: PetMood::Happy,
        seed: 7,
        accessory: Some("red_bow"),
        evolution: None,
    };
    let mut loader = PetAssetLoader::new(
        FilesystemPetAssets::new(pets),
        HybridPetRenderer::new(&components),
    );
    let first = loader.get_pet_card(&request);
    let second = loader.get_pet_card(&request);
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(
        first.filename, "pet_common_cama_adult_happy.png",
        "provider embeds this attachment as attachment://pet_common_cama_adult_happy.png"
    );
    let info = inspect_png(first.bytes()).expect("pet fixture must be a valid RGBA PNG");
    assert_eq!((info.width, info.height, info.color_type), (512, 288, 6));
    let raster = decode_png_raster(first.bytes()).expect("decode pet fixture PNG");
    assert_eq!((raster.width, raster.height), (512, 288));
    assert!(
        raster
            .pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3].iter().copied().max().unwrap_or(0) > 80)
            .count()
            > 10_000,
        "pet fixture must contain a visible foreground"
    );
    let procedural = render_pet_card(&request).encode_png();
    assert_ne!(
        first.bytes(),
        procedural,
        "fixture must exercise the checked-in HybridPetRenderer component path"
    );
}

#[test]
fn native_rating_history_fixture_render_is_deterministic_and_nonblank() {
    let fixture = fixture();
    let entries = fixture
        .rating_history
        .entries
        .iter()
        .map(|entry| RatingHistoryEntry {
            rating: entry.rating,
            openskill_mu: entry.os_mu_after,
            won: entry.won,
        })
        .collect::<Vec<_>>();
    let first = draw_rating_history_chart(&fixture.rating_history.username, &entries).into_inner();
    let second = draw_rating_history_chart(&fixture.rating_history.username, &entries).into_inner();
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("decode rating-history PNG");
    assert_eq!((raster.width, raster.height), (700, 400));
    assert!(
        raster
            .pixels
            .chunks_exact(4)
            .filter(|pixel| {
                pixel[..3].iter().copied().max().unwrap_or(0) > FOREGROUND_THRESHOLD
            })
            .count()
            > 1_000
    );
}

#[test]
fn native_rating_analysis_comparison_fixture_matches_live_attachment_contract() {
    let fixture = fixture();
    let comparison = &fixture.rating_analysis.comparison;
    let result = RatingComparisonResult {
        glicko: rating_stats("Glicko-2", comparison.matches_analyzed, &comparison.glicko),
        openskill: rating_stats(
            "OpenSkill",
            comparison.matches_analyzed,
            &comparison.openskill,
        ),
        matches_analyzed: comparison.matches_analyzed,
        match_data: Vec::new(),
    };
    let mut drawing = NativeRatingAnalysisDrawing;
    let first = drawing
        .rating_comparison_chart(&result)
        .expect("render rating-analysis comparison fixture");
    let mut drawing = NativeRatingAnalysisDrawing;
    let second = drawing
        .rating_comparison_chart(&result)
        .expect("render rating-analysis comparison fixture again");
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("decode rating-analysis comparison PNG");
    assert_eq!((raster.width, raster.height), (989, 413));
    for expected_color in [
        [88, 101, 242, 255],
        [87, 242, 135, 255],
        [254, 231, 92, 255],
    ] {
        assert!(
            raster
                .pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel == expected_color)
                .count()
                > 50,
            "rating-analysis comparison should preserve semantic chart color"
        );
    }
}

#[test]
fn native_rating_analysis_calibration_fixture_matches_live_attachment_contract() {
    let fixture = fixture();
    let curves = CalibrationCurveData {
        glicko: fixture
            .rating_analysis
            .calibration
            .glicko
            .iter()
            .map(|point| CalibrationPoint {
                predicted: point[0],
                actual_rate: point[1],
                count: point[2] as usize,
            })
            .collect(),
        openskill: fixture
            .rating_analysis
            .calibration
            .openskill
            .iter()
            .map(|point| CalibrationPoint {
                predicted: point[0],
                actual_rate: point[1],
                count: point[2] as usize,
            })
            .collect(),
        perfect_line: vec![(0.0, 0.0), (1.0, 1.0)],
    };
    let mut drawing = NativeRatingAnalysisDrawing;
    let first = drawing
        .calibration_curve_chart(&curves)
        .expect("render rating-analysis calibration fixture");
    let mut drawing = NativeRatingAnalysisDrawing;
    let second = drawing
        .calibration_curve_chart(&curves)
        .expect("render rating-analysis calibration fixture again");
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("decode rating-analysis calibration PNG");
    assert_eq!((raster.width, raster.height), (640, 490));
    for expected_color in [[88, 101, 242, 255], [87, 242, 135, 255]] {
        assert!(
            raster
                .pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel == expected_color)
                .count()
                > 50,
            "rating-analysis calibration should preserve semantic curve color"
        );
    }
}

#[test]
fn native_rating_analysis_trend_fixture_matches_live_attachment_contract() {
    let fixture = fixture();
    let trend = &fixture.rating_analysis.trend;
    let result = RatingComparisonResult {
        glicko: rating_stats(
            "Glicko-2",
            trend.match_data.len(),
            &fixture.rating_analysis.comparison.glicko,
        ),
        openskill: rating_stats(
            "OpenSkill",
            trend.match_data.len(),
            &fixture.rating_analysis.comparison.openskill,
        ),
        matches_analyzed: trend.match_data.len(),
        match_data: trend
            .match_data
            .iter()
            .enumerate()
            .map(|(index, row)| RatingComparisonMatchData {
                match_id: index as i64 + 1,
                match_date: index as i64,
                radiant_won: row.glicko_correct,
                glicko_radiant_prob: if row.glicko_correct { 0.75 } else { 0.25 },
                openskill_radiant_prob: if row.openskill_correct { 0.75 } else { 0.25 },
                raw_openskill_radiant_prob: if row.openskill_correct { 0.75 } else { 0.25 },
                glicko_correct: row.glicko_correct,
                openskill_correct: row.openskill_correct,
            })
            .collect(),
    };
    let mut drawing = NativeRatingAnalysisDrawing;
    let first = drawing
        .prediction_over_time_chart(&result, trend.window)
        .expect("render rating-analysis trend fixture");
    let mut drawing = NativeRatingAnalysisDrawing;
    let second = drawing
        .prediction_over_time_chart(&result, trend.window)
        .expect("render rating-analysis trend fixture again");
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("decode rating-analysis trend PNG");
    assert_eq!((raster.width, raster.height), (789, 390));
    for expected_color in [[88, 101, 242, 255], [87, 242, 135, 255]] {
        assert!(
            raster
                .pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel == expected_color)
                .count()
                > 50,
            "rating-analysis trend should preserve semantic series color"
        );
    }
}

#[test]
fn native_terminal_crash_fixture_render_has_exact_playback_contract() {
    let fixture = fixture();
    let first = GifAsset::terminal_crash(
        &fixture.terminal_crash.name,
        fixture.terminal_crash.filing_number,
    );
    let second = GifAsset::terminal_crash(
        &fixture.terminal_crash.name,
        fixture.terminal_crash.filing_number,
    );
    assert_eq!(first.bytes, second.bytes);
    assert_eq!(first.frame_durations_ms.len(), 58);
    assert_eq!(first.frame_durations_ms[..10], [120; 10]);
    assert_eq!(
        first.frame_durations_ms[10..30],
        [
            80, 80, 90, 90, 100, 100, 110, 110, 120, 120, 130, 130, 140, 140, 150, 150, 160, 160,
            170, 170
        ]
    );
    assert_eq!(first.frame_durations_ms[30..50], [60; 20]);
    assert_eq!(
        first.frame_durations_ms[50..],
        [1_100, 300, 300, 300, 300, 600, 300, 60_000]
    );
    assert!(first.shared_palette);
    assert_eq!(first.kind, "terminal_crash");

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(first.bytes))
        .expect("decode fixture terminal crash GIF");
    let mut durations = Vec::new();
    let mut frame_count = 0;
    while let Some(frame) = decoder
        .read_next_frame()
        .expect("read terminal crash frame")
    {
        durations.push(u32::from(frame.delay) * 10);
        frame_count += 1;
    }
    assert_eq!(frame_count, 58);
    assert_eq!(durations, first.frame_durations_ms);
}

#[test]
fn native_fixture_render_is_deterministic_and_seekable() {
    let fixture = fixture();
    let chart = &fixture.chart;
    let first_chart = draw_prediction_market_chart(
        chart.market_id,
        chart.title.as_deref(),
        &chart.snapshots,
        chart.created_at,
        chart.now,
    )
    .into_inner();
    let second_chart = draw_prediction_market_chart(
        chart.market_id,
        chart.title.as_deref(),
        &chart.snapshots,
        chart.created_at,
        chart.now,
    )
    .into_inner();
    assert_eq!(first_chart, second_chart);

    let balance = &fixture.balance;
    let series = balance
        .series
        .iter()
        .map(|(event_number, cumulative, source)| BalancePoint {
            event_number: *event_number,
            cumulative: *cumulative,
            source: source.clone(),
        })
        .collect::<Vec<_>>();
    let first_balance =
        draw_balance_chart(&balance.username, &series, &balance.source_totals).into_inner();
    let second_balance =
        draw_balance_chart(&balance.username, &series, &balance.source_totals).into_inner();
    assert_eq!(first_balance, second_balance);
    assert_eq!(&first_balance[..8], b"\x89PNG\r\n\x1a\n");

    let animation = &fixture.animation;
    let first = render_post_match_gif(&animation.name, animation.value, &animation.theme)
        .expect("render first fixture animation");
    let second = render_post_match_gif(&animation.name, animation.value, &animation.theme)
        .expect("render second fixture animation");
    assert_eq!(first.bytes, second.bytes);

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(first.bytes))
        .expect("decode fixture animation");
    let mut durations = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("read fixture frame") {
        durations.push(u32::from(frame.delay) * 10);
    }
    assert_eq!(durations, [vec![80; 17], vec![60_000]].concat());
}

#[test]
fn native_pinnacle_fixture_render_has_exact_phase_three_contract() {
    let fixture = fixture();
    let source = fs::read(fixture_source_path(&fixture.pinnacle.source_path))
        .expect("fixture pinnacle source image");
    let request = RenderRequest::PinnaclePhase {
        boss_id: fixture.pinnacle.boss_id.clone(),
        phase: 3,
        secret: fixture.pinnacle.secret,
    };
    let renderer = NativeDigRenderer;
    let first = renderer
        .render(&request, Some(&source))
        .expect("render fixture pinnacle phase three");
    let second = renderer
        .render(&request, Some(&source))
        .expect("render fixture pinnacle phase three again");
    assert_eq!(first.bytes, second.bytes);
    let info = inspect_media(&first.bytes).expect("inspect fixture pinnacle GIF");
    assert_eq!(info.format, MediaFormat::Gif);
    assert_eq!((info.width, info.height), (512, 288));
    assert_eq!(info.frame_count, 8);
    assert_eq!(info.loop_count, None);
    assert_eq!(
        info.frame_durations_ms,
        [90; 7].into_iter().chain([1_500]).collect::<Vec<_>>()
    );

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(first.bytes))
        .expect("decode fixture pinnacle GIF");
    let mut frame_count = 0;
    let mut first_frame = None;
    let mut durations = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("read pinnacle frame") {
        durations.push(u32::from(frame.delay) * 10);
        if first_frame.is_none() {
            first_frame = Some(frame.buffer.to_vec());
        }
        frame_count += 1;
    }
    assert_eq!(frame_count, 8);
    assert_eq!(
        durations,
        [90; 7].into_iter().chain([1_500]).collect::<Vec<_>>()
    );
    let first_frame = first_frame.expect("fixture pinnacle has a first frame");
    let (foreground_count, foreground_cells) = foreground_signature(&first_frame, 512, 288);
    assert!(foreground_count > 512 * 288 / 10);
    assert!(foreground_cells.len() >= 20);
}

#[test]
fn pixel_metrics_are_normalized_and_exact_for_identical_rgba() {
    assert_eq!(
        pixel_metrics(&[0, 10, 20, 255], &[0, 10, 20, 255]),
        (0.0, 0.0, 1.0)
    );
    assert_eq!(
        pixel_metrics(&[0, 0, 0, 255], &[255, 0, 0, 255]),
        (0.25, 0.5, 0.75)
    );
}

#[test]
fn foreground_gate_rejects_contentless_animation() {
    let animation = fixture().animation;
    let asset = render_post_match_gif(&animation.name, animation.value, &animation.theme)
        .expect("render fixture animation");
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(asset.bytes))
        .expect("decode fixture animation");
    let frame = decoder
        .read_next_frame()
        .expect("read fixture frame")
        .expect("fixture has a first frame");
    let reference = frame.buffer.to_vec();
    let blank = [5, 5, 8, 255].repeat(400 * 300);

    assert!(foreground_gate(&reference, &reference, 400, 300, 0.5, 0.65));
    assert!(!foreground_gate(&reference, &blank, 400, 300, 0.5, 0.65));
}

#[test]
fn balance_gate_rejects_contentless_candidate() {
    let fixture = fixture();
    let balance = fixture.balance;
    let series = balance
        .series
        .iter()
        .map(|(event_number, cumulative, source)| BalancePoint {
            event_number: *event_number,
            cumulative: *cumulative,
            source: source.clone(),
        })
        .collect::<Vec<_>>();
    let bytes = draw_balance_chart(&balance.username, &series, &balance.source_totals).into_inner();
    let image = decode_png_raster(&bytes).expect("decode fixture balance chart");
    let width = image.width;
    let height = image.height;
    let reference = image.pixels;
    let blank = [5, 5, 8, 255].repeat(width * height);
    let mut border = blank.clone();
    for y in 0..height {
        for x in 0..width {
            if x < 3 || x >= width - 3 || y < 3 || y >= height - 3 {
                let offset = (y * width + x) * 4;
                border[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }

    assert!(foreground_gate(
        &reference, &reference, width, height, 0.8, 0.8,
    ));
    assert!(!foreground_gate(
        &reference, &blank, width, height, 0.8, 0.8,
    ));
    assert!(!foreground_gate(
        &reference, &border, width, height, 0.8, 0.8,
    ));
}
