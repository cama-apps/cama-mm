use std::collections::{BTreeMap, BTreeSet};
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
    AdvantageData, BalancePoint, GambaInfo, GambaPoint, GambaStats, HeroPerformanceEntry, MatchRow,
    RatingHistoryEntry, draw_advantage_graph, draw_balance_chart, draw_gamba_chart,
    draw_hero_performance_chart, draw_lane_distribution, draw_matches_table,
    draw_prediction_market_chart, draw_rating_distribution_with_median, draw_rating_history_chart,
    draw_role_graph,
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

const FIXTURE_JSON: &str = include_str!("fixtures/visual_equivalence.json");
const FOREGROUND_THRESHOLD: u8 = 80;

#[derive(Debug, Deserialize)]
struct Fixture {
    chart: ChartFixture,
    animation: AnimationFixture,
    terminal_crash: TerminalCrashFixture,
    pinnacle: PinnacleFixture,
    balance: BalanceFixture,
    gamba: GambaFixture,
    rating_history: RatingHistoryFixture,
    rating_distribution: RatingDistributionFixture,
    rating_analysis: RatingAnalysisFixture,
    advantage: AdvantageFixture,
    pet: PetFixture,
    blame_luke: BlameLukeFixture,
    scout: ScoutFixture,
    hero_grid: HeroGridFixture,
    profile: ProfileFixture,
}

#[derive(Debug, Deserialize)]
struct BalanceFixture {
    username: String,
    series: Vec<(i32, i64, String)>,
    source_totals: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Deserialize)]
struct GambaFixture {
    username: String,
    degen_score: i32,
    degen_title: String,
    series: Vec<GambaPointFixture>,
    stats: GambaStatsFixture,
}

#[derive(Debug, Deserialize)]
struct GambaPointFixture {
    event_number: i32,
    cumulative: i64,
    source: String,
    outcome: Option<String>,
    leverage: i64,
    profit: i64,
}

#[derive(Debug, Deserialize)]
struct GambaStatsFixture {
    total_bets: usize,
    win_rate: f64,
    net_pnl: i64,
    roi: f64,
}

#[derive(Debug, Deserialize)]
struct RatingHistoryFixture {
    username: String,
    entries: Vec<RatingHistoryEntryFixture>,
}

#[derive(Debug, Deserialize)]
struct RatingDistributionFixture {
    ratings: Vec<f64>,
    median_rating: Option<f64>,
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
struct ProfileFixture {
    username: String,
    roles: BTreeMap<String, f64>,
    lanes: Vec<ProfileLaneFixture>,
    hero_performance: Vec<ProfileHeroFixture>,
    recent_matches: Vec<ProfileMatchFixture>,
}

#[derive(Debug, Deserialize)]
struct ProfileLaneFixture {
    name: String,
    value: f64,
}

#[derive(Debug, Deserialize)]
struct ProfileHeroFixture {
    hero_id: i64,
    games: i64,
    wins: i64,
}

#[derive(Debug, Deserialize)]
struct ProfileMatchFixture {
    hero_id: Option<i64>,
    hero_name: Option<String>,
    kills: i64,
    deaths: i64,
    assists: i64,
    won: Option<bool>,
    duration: Option<u64>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GambaMarkerKind {
    Bet,
    Wheel,
    Leverage,
    DoubleOrNothing,
}

fn gamba_marker_kind_name(kind: GambaMarkerKind) -> &'static str {
    match kind {
        GambaMarkerKind::Bet => "bet",
        GambaMarkerKind::Wheel => "wheel",
        GambaMarkerKind::Leverage => "leverage",
        GambaMarkerKind::DoubleOrNothing => "double_or_nothing",
    }
}

#[derive(Clone, Copy, Debug)]
struct GambaMarkerSpec {
    center_x: i32,
    center_y: i32,
    kind: GambaMarkerKind,
    outcome: [u8; 3],
}

type MarkerCells = BTreeSet<(usize, u8, i32, i32)>;
type MarkerSignature = ([usize; 3], MarkerCells);

fn native_gamba_fixture() -> (Vec<u8>, Vec<GambaMarkerSpec>) {
    let fixture = fixture();
    let series = fixture
        .gamba
        .series
        .iter()
        .map(|point| GambaPoint {
            event_number: point.event_number,
            cumulative: point.cumulative,
            info: GambaInfo {
                source: point.source.clone(),
                outcome: point.outcome.clone(),
                leverage: point.leverage,
                profit: point.profit,
            },
        })
        .collect::<Vec<_>>();
    let stats = GambaStats {
        total_bets: fixture.gamba.stats.total_bets,
        win_rate: fixture.gamba.stats.win_rate,
        net_pnl: fixture.gamba.stats.net_pnl,
        roi: fixture.gamba.stats.roi,
    };
    let bytes = draw_gamba_chart(
        &fixture.gamba.username,
        fixture.gamba.degen_score,
        &fixture.gamba.degen_title,
        &series,
        stats,
    )
    .into_inner();
    let pixels = decode_png_raster(&bytes)
        .expect("native Gamba fixture must be a PNG")
        .pixels;

    let values = series
        .iter()
        .map(|point| point.cumulative as f64)
        .collect::<Vec<_>>();
    let logs = values
        .iter()
        .map(|value| value.signum() * value.abs().ln_1p())
        .collect::<Vec<_>>();
    let mut minimum = logs.iter().copied().fold(0.0_f64, f64::min);
    let mut maximum = logs.iter().copied().fold(0.0_f64, f64::max);
    let range = (maximum - minimum).abs().max(0.1);
    minimum -= range * 0.1;
    maximum += range * 0.1;
    let log_span = (maximum - minimum).max(1e-9);
    let specs = series
        .iter()
        .map(|point| {
            let center_x = 60
                + (((point.event_number - 1) as f64
                    / (series.len().saturating_sub(1).max(1) as f64))
                    * 614.0) as i32;
            let signed =
                point.cumulative.signum() as f64 * (point.cumulative.unsigned_abs() as f64).ln_1p();
            let center_y = 88 + (((maximum - signed) / log_span) * 222.0) as i32;
            let kind = if point.info.source == "double_or_nothing" {
                GambaMarkerKind::DoubleOrNothing
            } else if point.info.source == "wheel" {
                GambaMarkerKind::Wheel
            } else if point.info.leverage.max(1) > 1 {
                GambaMarkerKind::Leverage
            } else {
                GambaMarkerKind::Bet
            };
            let outcome = match point.info.outcome.as_deref() {
                Some("won") => [87, 242, 135],
                Some("neutral") => [185, 187, 190],
                _ => [237, 66, 69],
            };
            GambaMarkerSpec {
                center_x,
                center_y,
                kind,
                outcome,
            }
        })
        .collect();
    (pixels, specs)
}

fn color_distance(pixel: &[u8], expected: [u8; 3]) -> u32 {
    pixel[..3]
        .iter()
        .zip(expected)
        .map(|(actual, target)| u32::from(actual.abs_diff(target)))
        .sum()
}

fn semantic_mask(
    pixels: &[u8],
    width: usize,
    expected: [u8; 3],
    region: (usize, usize, usize, usize),
    distance: u32,
    grid: (usize, usize),
) -> (usize, BTreeSet<(usize, usize)>) {
    let (left, top, right, bottom) = region;
    let mut count = 0;
    let mut cells = BTreeSet::new();
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        let x = index % width;
        let y = index / width;
        if x < left || x >= right || y < top || y >= bottom {
            continue;
        }
        if color_distance(pixel, expected) <= distance {
            count += 1;
            cells.insert((x * grid.0 / width, y * grid.1 / (pixels.len() / width / 4)));
        }
    }
    (count, cells)
}

fn marker_signature(
    pixels: &[u8],
    width: usize,
    height: usize,
    specs: &[GambaMarkerSpec],
) -> MarkerSignature {
    let mut counts = [0_usize; 3];
    let mut cells = BTreeSet::new();
    for (marker_index, spec) in specs.iter().enumerate() {
        let radius = 7;
        for y in (spec.center_y - radius).max(0)..=(spec.center_y + radius).min(height as i32 - 1) {
            for x in
                (spec.center_x - radius).max(0)..=(spec.center_x + radius).min(width as i32 - 1)
            {
                let offset = (y as usize * width + x as usize) * 4;
                let pixel = &pixels[offset..offset + 4];
                let role = if color_distance(pixel, spec.outcome) <= 55 {
                    Some(0_u8)
                } else if color_distance(pixel, [47, 49, 54]) <= 18 {
                    Some(1_u8)
                } else if color_distance(pixel, [255, 255, 255]) <= 40 {
                    Some(2_u8)
                } else {
                    None
                };
                if let Some(role) = role {
                    counts[role as usize] += 1;
                    cells.insert((
                        marker_index,
                        role,
                        (x - spec.center_x + radius) / 2,
                        (y - spec.center_y + radius) / 2,
                    ));
                }
            }
        }
    }
    (counts, cells)
}

fn marker_kind_contract(
    reference: &[u8],
    candidate: &[u8],
    width: usize,
    height: usize,
    specs: &[GambaMarkerSpec],
    kind: GambaMarkerKind,
) -> Result<(), String> {
    let selected = specs
        .iter()
        .copied()
        .filter(|spec| spec.kind == kind)
        .collect::<Vec<_>>();
    let reference_signature = marker_signature(reference, width, height, &selected);
    let candidate_signature = marker_signature(candidate, width, height, &selected);
    let required_roles: &[usize] = match kind {
        GambaMarkerKind::Bet => &[0],
        GambaMarkerKind::Wheel => &[0, 1, 2],
        GambaMarkerKind::Leverage | GambaMarkerKind::DoubleOrNothing => &[0, 1],
    };
    for &role in required_roles {
        let reference_count = reference_signature.0[role];
        let candidate_count = candidate_signature.0[role];
        let ratio = candidate_count as f64 / reference_count.max(1) as f64;
        if reference_count == 0 || ratio < 0.18 {
            return Err(format!(
                "{} marker role {role} occupancy is missing",
                gamba_marker_kind_name(kind)
            ));
        }
    }
    if kind != GambaMarkerKind::Wheel && candidate_signature.0[2] > reference_signature.0[2] + 2 {
        return Err(format!(
            "{} marker gained forbidden wheel spokes",
            gamba_marker_kind_name(kind)
        ));
    }
    let intersection = reference_signature
        .1
        .intersection(&candidate_signature.1)
        .count();
    let union = reference_signature.1.union(&candidate_signature.1).count();
    let iou = intersection as f64 / union.max(1) as f64;
    if iou < 0.60 {
        return Err(format!(
            "{} marker shape drifted: IoU {iou:.3}",
            gamba_marker_kind_name(kind)
        ));
    }
    Ok(())
}

fn gamba_contract(
    reference: &[u8],
    candidate: &[u8],
    width: usize,
    height: usize,
    specs: &[GambaMarkerSpec],
) -> Result<(), String> {
    if (width, height) != (700, 400) || candidate.len() != width * height * 4 {
        return Err("profile gamba dimensions differ".to_owned());
    }
    let (reference_fill, reference_cells) = semantic_mask(
        reference,
        width,
        [59, 85, 74],
        (60, 88, 674, 310),
        3,
        (14, 8),
    );
    let (candidate_fill, candidate_cells) = semantic_mask(
        candidate,
        width,
        [59, 85, 74],
        (60, 88, 674, 310),
        3,
        (14, 8),
    );
    let fill_ratio = candidate_fill as f64 / reference_fill.max(1) as f64;
    let fill_union = reference_cells.union(&candidate_cells).count();
    let fill_iou =
        reference_cells.intersection(&candidate_cells).count() as f64 / fill_union.max(1) as f64;
    if reference_fill == 0 || fill_ratio < 0.35 || fill_iou < 0.45 {
        return Err(format!(
            "positive fill layer is missing: ratio {fill_ratio:.3}, IoU {fill_iou:.3}"
        ));
    }
    for kind in [
        GambaMarkerKind::Bet,
        GambaMarkerKind::Wheel,
        GambaMarkerKind::Leverage,
        GambaMarkerKind::DoubleOrNothing,
    ] {
        marker_kind_contract(reference, candidate, width, height, specs, kind)?;
    }
    Ok(())
}

fn rating_distribution_contract(
    reference: &[u8],
    candidate: &[u8],
    width: usize,
    height: usize,
) -> Result<(), String> {
    if (width, height) != (640, 390) || candidate.len() != width * height * 4 {
        return Err("rating distribution dimensions differ".to_owned());
    }
    for (name, color) in [
        ("histogram", [88, 101, 242]),
        ("normal", [87, 242, 135]),
        ("kde", [254, 231, 92]),
        ("mean", [237, 66, 69]),
        ("median", [244, 123, 103]),
    ] {
        let (reference_count, reference_cells) =
            semantic_mask(reference, width, color, (0, 0, width, height), 60, (16, 10));
        let (candidate_count, candidate_cells) =
            semantic_mask(candidate, width, color, (0, 0, width, height), 60, (16, 10));
        if reference_count < 12 || reference_cells.is_empty() {
            return Err(format!("rating distribution reference lost {name}"));
        }
        let ratio = candidate_count as f64 / reference_count as f64;
        let union = reference_cells.union(&candidate_cells).count();
        let iou =
            reference_cells.intersection(&candidate_cells).count() as f64 / union.max(1) as f64;
        if candidate_count < 12 || ratio < 0.20 || iou < 0.82 {
            return Err(format!(
                "rating distribution {name} is missing or misplaced: ratio {ratio:.3}, IoU {iou:.3}"
            ));
        }
    }
    Ok(())
}

fn paint_bg_circle(pixels: &mut [u8], width: usize, center_x: i32, center_y: i32, radius: i32) {
    for y in -radius..=radius {
        for x in -radius..=radius {
            if x * x + y * y > radius * radius {
                continue;
            }
            let px = center_x + x;
            let py = center_y + y;
            if px < 0
                || py < 0
                || px >= width as i32
                || py >= pixels.len() as i32 / width as i32 / 4
            {
                continue;
            }
            let offset = (py as usize * width + px as usize) * 4;
            pixels[offset..offset + 4].copy_from_slice(&[54, 57, 63, 255]);
        }
    }
}

fn paint_marker_shape(
    pixels: &mut [u8],
    width: usize,
    spec: GambaMarkerSpec,
    replacement: GambaMarkerKind,
) {
    let radius = match replacement {
        GambaMarkerKind::Bet => 3,
        GambaMarkerKind::Wheel | GambaMarkerKind::Leverage => 5,
        GambaMarkerKind::DoubleOrNothing => 6,
    };
    let mut set = |x: i32, y: i32, color: [u8; 4]| {
        if x < 0 || y < 0 {
            return;
        }
        let height = pixels.len() / width / 4;
        if x >= width as i32 || y >= height as i32 {
            return;
        }
        let offset = (y as usize * width + x as usize) * 4;
        pixels[offset..offset + 4].copy_from_slice(&color);
    };
    match replacement {
        GambaMarkerKind::Bet => {
            for y in -radius..=radius {
                for x in -radius..=radius {
                    if x * x + y * y <= radius * radius {
                        set(
                            spec.center_x + x,
                            spec.center_y + y,
                            [spec.outcome[0], spec.outcome[1], spec.outcome[2], 255],
                        );
                    }
                }
            }
        }
        GambaMarkerKind::Leverage => {
            for y in -radius..=radius {
                for x in -radius..=radius {
                    if x.abs() + y.abs() <= radius {
                        let color = if x.abs() + y.abs() == radius {
                            [47, 49, 54, 255]
                        } else {
                            [spec.outcome[0], spec.outcome[1], spec.outcome[2], 255]
                        };
                        set(spec.center_x + x, spec.center_y + y, color);
                    }
                }
            }
        }
        GambaMarkerKind::Wheel => {
            for y in -radius..=radius {
                for x in -radius..=radius {
                    if x * x + y * y <= radius * radius {
                        let color = if x.abs() == radius || y.abs() == radius {
                            [47, 49, 54, 255]
                        } else {
                            [spec.outcome[0], spec.outcome[1], spec.outcome[2], 255]
                        };
                        set(spec.center_x + x, spec.center_y + y, color);
                    }
                }
            }
            for offset in -3..=3 {
                set(spec.center_x + offset, spec.center_y, [255, 255, 255, 255]);
                set(spec.center_x, spec.center_y + offset, [255, 255, 255, 255]);
            }
        }
        GambaMarkerKind::DoubleOrNothing => {
            let points = (0..16)
                .map(|index| {
                    let angle =
                        index as f64 * std::f64::consts::PI / 8.0 - std::f64::consts::FRAC_PI_2;
                    let point_radius = if index % 2 == 0 { 6.0 } else { 6.0 * 0.42 };
                    (
                        spec.center_x + (point_radius * angle.cos()) as i32,
                        spec.center_y + (point_radius * angle.sin()) as i32,
                    )
                })
                .collect::<Vec<_>>();
            for y in -radius..=radius {
                for x in -radius..=radius {
                    let point_x = spec.center_x + x;
                    let point_y = spec.center_y + y;
                    let mut inside = false;
                    for (first, second) in points
                        .iter()
                        .zip(points.iter().cycle().skip(1))
                        .take(points.len())
                    {
                        if (first.1 > point_y) != (second.1 > point_y)
                            && (point_x as f64)
                                < (second.0 - first.0) as f64 * (point_y - first.1) as f64
                                    / (second.1 - first.1) as f64
                                    + first.0 as f64
                        {
                            inside = !inside;
                        }
                    }
                    if inside {
                        set(
                            point_x,
                            point_y,
                            [spec.outcome[0], spec.outcome[1], spec.outcome[2], 255],
                        );
                    }
                }
            }
            for (first, second) in points
                .iter()
                .zip(points.iter().cycle().skip(1))
                .take(points.len())
            {
                let steps = (second.0 - first.0)
                    .abs()
                    .max((second.1 - first.1).abs())
                    .max(1);
                for step in 0..=steps {
                    let x = first.0 + (second.0 - first.0) * step / steps;
                    let y = first.1 + (second.1 - first.1) * step / steps;
                    set(x, y, [47, 49, 54, 255]);
                }
            }
        }
    }
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
    assert_eq!(
        fixture.rating_distribution.ratings,
        vec![1_400.0, 1_500.0, 1_520.0, 1_600.0, 1_700.0, 1_450.0]
    );
    assert_eq!(fixture.rating_distribution.median_rating, Some(1_510.0));
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
    assert_eq!(fixture.profile.username, "Visual Profile");
    assert_eq!(fixture.profile.roles["Carry"], 50.0);
    assert_eq!(fixture.profile.roles["Support"], 30.0);
    assert_eq!(fixture.profile.roles["Nuker"], 20.0);
    assert_eq!(fixture.profile.lanes.len(), 5);
    assert_eq!(
        fixture
            .profile
            .lanes
            .iter()
            .map(|lane| (lane.name.as_str(), lane.value as i32))
            .collect::<Vec<_>>(),
        vec![
            ("Roaming", 20),
            ("Safe Lane", 30),
            ("Mid", 25),
            ("Off Lane", 15),
            ("Jungle", 10),
        ]
    );
    assert_eq!(
        fixture
            .profile
            .hero_performance
            .iter()
            .map(|hero| (hero.games, hero.wins))
            .collect::<Vec<_>>(),
        vec![(8, 5), (6, 3), (5, 2), (4, 1)]
    );
    assert_eq!(fixture.profile.recent_matches.len(), 3);
    assert_eq!(fixture.profile.recent_matches[0].hero_id, Some(76));
    assert_eq!(
        fixture.profile.recent_matches[0].hero_name.as_deref(),
        Some("Outworld Destroyer")
    );
    assert_eq!(fixture.profile.recent_matches[0].duration, Some(2_400));
    assert_eq!(fixture.profile.recent_matches[2].won, None);
    assert_eq!(fixture.profile.recent_matches[2].duration, None);
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
fn native_profile_fixture_render_preserves_rows_geometry_and_semantic_colors() {
    let fixture = fixture();
    let profile = &fixture.profile;
    let role_first =
        draw_role_graph(&profile.roles, &format!("Roles: {}", profile.username)).into_inner();
    let role_second =
        draw_role_graph(&profile.roles, &format!("Roles: {}", profile.username)).into_inner();
    assert_eq!(role_first, role_second);
    let role = decode_png_raster(&role_first).expect("role graph fixture must be a PNG");
    assert_eq!((role.width, role.height), (400, 400));
    for expected_color in [
        [88, 101, 242, 255],
        [255, 255, 255, 255],
        [185, 187, 190, 255],
    ] {
        assert!(
            role.pixels
                .chunks_exact(4)
                .any(|pixel| pixel == expected_color),
            "role graph should preserve labels, scale annotations, and accent polygon"
        );
    }

    let lanes = profile
        .lanes
        .iter()
        .map(|lane| (lane.name.clone(), lane.value))
        .collect::<Vec<_>>();
    let lane = decode_png_raster(&draw_lane_distribution(&lanes).into_inner())
        .expect("lane distribution fixture must be a PNG");
    assert_eq!((lane.width, lane.height), (350, 260));
    for expected_color in [
        [233, 30, 99, 255],
        [76, 175, 80, 255],
        [33, 150, 243, 255],
        [255, 152, 0, 255],
        [156, 39, 176, 255],
    ] {
        assert!(
            lane.pixels
                .chunks_exact(4)
                .any(|pixel| pixel == expected_color),
            "lane distribution should preserve each ordered lane color"
        );
    }

    let heroes = profile
        .hero_performance
        .iter()
        .map(|hero| HeroPerformanceEntry {
            hero_name: cama_app::hero_lookup::hero_name(hero.hero_id),
            games: hero.games,
            wins: hero.wins,
        })
        .collect::<Vec<_>>();
    let hero =
        decode_png_raster(&draw_hero_performance_chart(&heroes, &profile.username).into_inner())
            .expect("hero performance fixture must be a PNG");
    assert_eq!((hero.width, hero.height), (450, 217));
    for expected_color in [
        [87, 242, 135, 255],
        [124, 179, 66, 255],
        [254, 231, 92, 255],
        [237, 66, 69, 255],
    ] {
        assert!(
            hero.pixels
                .chunks_exact(4)
                .any(|pixel| pixel == expected_color),
            "hero performance should preserve every win-rate bracket"
        );
    }

    let recent = profile
        .recent_matches
        .iter()
        .map(|row| MatchRow {
            hero_id: row.hero_id,
            hero_name: row.hero_name.clone(),
            kills: row.kills,
            deaths: row.deaths,
            assists: row.assists,
            won: row.won,
            duration_seconds: row.duration,
        })
        .collect::<Vec<_>>();
    let recent = decode_png_raster(
        &draw_matches_table(&recent, &std::collections::BTreeMap::new()).into_inner(),
    )
    .expect("recent matches fixture must be a PNG");
    assert_eq!((recent.width, recent.height), (370, 160));
    for expected_color in [
        [88, 101, 242, 255],
        [87, 242, 135, 255],
        [237, 66, 69, 255],
        [185, 187, 190, 255],
    ] {
        assert!(
            recent
                .pixels
                .chunks_exact(4)
                .any(|pixel| pixel == expected_color),
            "recent match table should preserve result/duration semantics"
        );
    }
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
fn native_profile_gamba_fixture_matches_live_attachment_contract() {
    let fixture = fixture();
    let series = fixture
        .gamba
        .series
        .iter()
        .map(|point| GambaPoint {
            event_number: point.event_number,
            cumulative: point.cumulative,
            info: GambaInfo {
                source: point.source.clone(),
                outcome: point.outcome.clone(),
                leverage: point.leverage,
                profit: point.profit,
            },
        })
        .collect::<Vec<_>>();
    let stats = GambaStats {
        total_bets: fixture.gamba.stats.total_bets,
        win_rate: fixture.gamba.stats.win_rate,
        net_pnl: fixture.gamba.stats.net_pnl,
        roi: fixture.gamba.stats.roi,
    };
    let first = draw_gamba_chart(
        &fixture.gamba.username,
        fixture.gamba.degen_score,
        &fixture.gamba.degen_title,
        &series,
        stats,
    )
    .into_inner();
    let second = draw_gamba_chart(
        &fixture.gamba.username,
        fixture.gamba.degen_score,
        &fixture.gamba.degen_title,
        &series,
        stats,
    )
    .into_inner();
    assert_eq!(first, second);
    let raster = decode_png_raster(&first).expect("decode profile Gamba PNG");
    assert_eq!((raster.width, raster.height), (700, 400));
    for expected in [
        [87, 242, 135, 255],
        [237, 66, 69, 255],
        [185, 187, 190, 255],
        [47, 49, 54, 255],
    ] {
        assert!(
            raster.pixels.chunks_exact(4).any(|pixel| pixel == expected),
            "Gamba fixture should preserve semantic color {expected:?}"
        );
    }
}

#[test]
fn test_profile_gamba_gate_rejects_missing_positive_fill() {
    let (reference, specs) = native_gamba_fixture();
    let pristine = gamba_contract(&reference, &reference, 700, 400, &specs);
    assert!(pristine.is_ok(), "{pristine:?}");

    let mut missing_fill = reference.clone();
    for (index, pixel) in missing_fill.chunks_exact_mut(4).enumerate() {
        let x = index % 700;
        let y = index / 700;
        if (60..674).contains(&x)
            && (88..310).contains(&y)
            && color_distance(pixel, [59, 85, 74]) <= 3
        {
            // Keep the chart occupied while removing only the authored green
            // area layer, matching the Python gate's semantic mutation.
            pixel.copy_from_slice(&[88, 101, 242, 255]);
        }
    }
    assert!(
        gamba_contract(&reference, &missing_fill, 700, 400, &specs)
            .expect_err("missing positive fill must be rejected")
            .contains("positive fill")
    );
}

#[test]
fn test_profile_gamba_gate_rejects_erased_and_substituted_marker_shapes() {
    let (reference, specs) = native_gamba_fixture();
    let pristine = gamba_contract(&reference, &reference, 700, 400, &specs);
    assert!(pristine.is_ok(), "{pristine:?}");

    for kind in [
        GambaMarkerKind::Bet,
        GambaMarkerKind::Wheel,
        GambaMarkerKind::Leverage,
        GambaMarkerKind::DoubleOrNothing,
    ] {
        for substituted in [false, true] {
            let mut candidate = reference.clone();
            for spec in specs.iter().copied().filter(|spec| spec.kind == kind) {
                paint_bg_circle(&mut candidate, 700, spec.center_x, spec.center_y, 7);
                if substituted {
                    for y in -7..=7 {
                        for x in -7..=7 {
                            if x * x + y * y <= 49 {
                                let px = spec.center_x + x;
                                let py = spec.center_y + y;
                                let offset = (py as usize * 700 + px as usize) * 4;
                                candidate[offset..offset + 4].copy_from_slice(&[88, 101, 242, 255]);
                            }
                        }
                    }
                }
            }
            let error = gamba_contract(&reference, &candidate, 700, 400, &specs)
                .expect_err("marker mutation should be rejected");
            assert!(
                error.contains(&format!("{} marker", gamba_marker_kind_name(kind))),
                "{kind:?} mutation should fail its own marker contract: {error}"
            );
        }
    }
}

#[test]
fn test_profile_gamba_gate_rejects_every_cross_kind_marker_substitution() {
    let (reference, specs) = native_gamba_fixture();
    let pristine = gamba_contract(&reference, &reference, 700, 400, &specs);
    assert!(pristine.is_ok(), "{pristine:?}");
    let kinds = [
        GambaMarkerKind::Bet,
        GambaMarkerKind::Wheel,
        GambaMarkerKind::Leverage,
        GambaMarkerKind::DoubleOrNothing,
    ];
    for target_kind in kinds {
        for replacement_kind in kinds {
            if target_kind == replacement_kind {
                continue;
            }
            let mut candidate = reference.clone();
            for spec in specs
                .iter()
                .copied()
                .filter(|spec| spec.kind == target_kind)
            {
                paint_bg_circle(&mut candidate, 700, spec.center_x, spec.center_y, 7);
                paint_marker_shape(&mut candidate, 700, spec, replacement_kind);
            }
            let error = gamba_contract(&reference, &candidate, 700, 400, &specs)
                .expect_err("cross-kind marker substitution should be rejected");
            assert!(
                error.contains(&format!("{} marker", gamba_marker_kind_name(target_kind))),
                "{target_kind:?} replaced by {replacement_kind:?} should fail its own marker contract: {error}"
            );
        }
    }
}

#[test]
fn native_rating_distribution_fixture_preserves_geometry_and_semantic_layers() {
    let fixture = fixture();
    let distribution = &fixture.rating_distribution;
    let first =
        draw_rating_distribution_with_median(&distribution.ratings, distribution.median_rating)
            .into_inner();
    let second =
        draw_rating_distribution_with_median(&distribution.ratings, distribution.median_rating)
            .into_inner();
    assert_eq!(first, second);

    let raster = decode_png_raster(&first).expect("decode rating-distribution PNG");
    assert_eq!((raster.width, raster.height), (640, 390));
    for expected_color in [
        [88, 101, 242, 255],
        [87, 242, 135, 255],
        [254, 231, 92, 255],
        [237, 66, 69, 255],
        [244, 123, 103, 255],
    ] {
        assert!(
            raster
                .pixels
                .chunks_exact(4)
                .any(|pixel| pixel == expected_color),
            "rating distribution should preserve every semantic layer"
        );
    }
    assert!(
        (130..330).any(|y| {
            (242..249).any(|x| {
                let offset = (y * raster.width as usize + x) * 4;
                raster.pixels[offset..offset + 4] == [244, 123, 103, 255]
            })
        }),
        "the explicit 1510 median should appear at its plot coordinate"
    );

    let without_median = draw_rating_distribution_with_median(&distribution.ratings, None);
    let without_median = decode_png_raster(without_median.get_ref())
        .expect("decode rating distribution without median");
    assert!(
        !without_median
            .pixels
            .chunks_exact(4)
            .any(|pixel| pixel == [244, 123, 103, 255]),
        "the explicit None median must suppress the marker and legend"
    );
}

#[test]
fn test_rating_distribution_gate_rejects_missing_median_and_wrong_geometry() {
    let fixture = fixture();
    let reference = decode_png_raster(
        &draw_rating_distribution_with_median(
            &fixture.rating_distribution.ratings,
            fixture.rating_distribution.median_rating,
        )
        .into_inner(),
    )
    .expect("rating distribution fixture must be a PNG")
    .pixels;
    let pristine = rating_distribution_contract(&reference, &reference, 640, 390);
    assert!(pristine.is_ok(), "{pristine:?}");

    let mut missing_median = reference.clone();
    for pixel in missing_median.chunks_exact_mut(4) {
        if color_distance(pixel, [244, 123, 103]) <= 1 {
            pixel.copy_from_slice(&[47, 49, 54, 255]);
        }
    }
    assert!(
        rating_distribution_contract(&reference, &missing_median, 640, 390)
            .expect_err("missing median must be rejected")
            .contains("median")
    );

    let mut wrong_geometry = Vec::with_capacity(639 * 390 * 4);
    for row in reference.chunks_exact(640 * 4) {
        wrong_geometry.extend_from_slice(&row[..639 * 4]);
    }
    assert!(
        rating_distribution_contract(&reference, &wrong_geometry, 639, 390)
            .expect_err("wrong geometry must be rejected")
            .contains("dimensions")
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
