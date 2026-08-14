//! Emit deterministic artifacts for the cross-language visual-equivalence gate.
//!
//! This is deliberately an example rather than a production path.  The
//! companion `scripts/visual_equivalence.py` owns the Python invocation and
//! image decoding; this binary only exercises the same public Rust renderers
//! that the runtime calls.

use std::env;
use std::fs;
use std::path::Path;

use cama_app::blame_luke_media::render_blame_luke;
use cama_app::dig_assets::{DigRenderPort, RenderRequest};
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
    FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest,
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
use serde::Deserialize;

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
    roles: std::collections::BTreeMap<String, f64>,
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
    snapshots: Vec<[i64; 2]>,
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

fn resolve_fixture_path(fixture_path: &Path, relative_path: &str) -> std::path::PathBuf {
    fixture_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(relative_path)
}

fn pet_stage(value: &str) -> Result<PetStage, &'static str> {
    match value {
        "adult" => Ok(PetStage::Adult),
        "baby" => Ok(PetStage::Baby),
        "egg" => Ok(PetStage::Egg),
        _ => Err("unsupported pet stage"),
    }
}

fn pet_mood(value: &str) -> Result<PetMood, &'static str> {
    match value {
        "happy" => Ok(PetMood::Happy),
        "neutral" | "content" => Ok(PetMood::Content),
        "hungry" | "starving" => Ok(PetMood::Hungry),
        _ => Err("unsupported pet mood"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let fixture_path = arguments.next().ok_or("missing fixture path")?;
    let output_dir = arguments.next().ok_or("missing output directory")?;
    if arguments.next().is_some() {
        return Err("usage: visual_equivalence <fixture.json> <output-dir>".into());
    }

    let fixture: Fixture = serde_json::from_slice(&fs::read(&fixture_path)?)?;
    fs::create_dir_all(Path::new(&output_dir))?;

    let snapshots = fixture
        .chart
        .snapshots
        .iter()
        .map(|snapshot| (snapshot[0], snapshot[1]))
        .collect::<Vec<_>>();
    let chart = draw_prediction_market_chart(
        fixture.chart.market_id,
        fixture.chart.title.as_deref(),
        &snapshots,
        fixture.chart.created_at,
        fixture.chart.now,
    )
    .into_inner();
    fs::write(Path::new(&output_dir).join("rust_chart.png"), chart)?;

    let balance = fixture
        .balance
        .series
        .iter()
        .map(|(event_number, cumulative, source)| BalancePoint {
            event_number: *event_number,
            cumulative: *cumulative,
            source: source.clone(),
        })
        .collect::<Vec<_>>();
    let balance_chart = draw_balance_chart(
        &fixture.balance.username,
        &balance,
        &fixture.balance.source_totals,
    )
    .into_inner();
    fs::write(
        Path::new(&output_dir).join("rust_balance.png"),
        balance_chart,
    )?;

    // This is the live `/profile` Gambling attachment boundary.  `/wrapped`
    // intentionally remains a separate renderer and is not exercised here.
    let gamba = &fixture.gamba;
    let gamba_series = gamba
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
    let gamba_chart = draw_gamba_chart(
        &gamba.username,
        gamba.degen_score,
        &gamba.degen_title,
        &gamba_series,
        GambaStats {
            total_bets: gamba.stats.total_bets,
            win_rate: gamba.stats.win_rate,
            net_pnl: gamba.stats.net_pnl,
            roi: gamba.stats.roi,
        },
    )
    .into_inner();
    fs::write(
        Path::new(&output_dir).join("rust_profile_gamba.png"),
        gamba_chart,
    )?;

    let rating_history = fixture
        .rating_history
        .entries
        .iter()
        .map(|entry| RatingHistoryEntry {
            rating: entry.rating,
            openskill_mu: entry.os_mu_after,
            won: entry.won,
        })
        .collect::<Vec<_>>();
    let rating_history_chart =
        draw_rating_history_chart(&fixture.rating_history.username, &rating_history).into_inner();
    fs::write(
        Path::new(&output_dir).join("rust_rating_history.png"),
        rating_history_chart,
    )?;

    // Exercise the exact native helper called by the live Rust calibration
    // provider, including the median computed by the service layer.
    let rating_distribution = draw_rating_distribution_with_median(
        &fixture.rating_distribution.ratings,
        fixture.rating_distribution.median_rating,
    )
    .into_inner();
    fs::write(
        Path::new(&output_dir).join("rust_rating_distribution.png"),
        rating_distribution,
    )?;

    // Exercise the same native renderer selected by the live
    // RatingAnalysisDrawingPort in the Rust `/ratinganalysis` provider.  The
    // fixture contains the comparison service payload at the Python command's
    // drawing boundary, so neither side needs a database or Discord adapter.
    let comparison = &fixture.rating_analysis.comparison;
    let comparison_result = RatingComparisonResult {
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
    let rating_analysis_chart = drawing.rating_comparison_chart(&comparison_result)?;
    fs::write(
        Path::new(&output_dir).join("rust_rating_analysis_comparison.png"),
        rating_analysis_chart,
    )?;

    let calibration = &fixture.rating_analysis.calibration;
    let calibration_data = CalibrationCurveData {
        glicko: calibration
            .glicko
            .iter()
            .map(|point| CalibrationPoint {
                predicted: point[0],
                actual_rate: point[1],
                count: point[2] as usize,
            })
            .collect(),
        openskill: calibration
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
    let calibration_chart = drawing.calibration_curve_chart(&calibration_data)?;
    fs::write(
        Path::new(&output_dir).join("rust_rating_analysis_calibration.png"),
        calibration_chart,
    )?;

    let trend = &fixture.rating_analysis.trend;
    let trend_result = RatingComparisonResult {
        glicko: rating_stats("Glicko-2", trend.match_data.len(), &comparison.glicko),
        openskill: rating_stats("OpenSkill", trend.match_data.len(), &comparison.openskill),
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
    let trend_chart = drawing.prediction_over_time_chart(&trend_result, trend.window)?;
    fs::write(
        Path::new(&output_dir).join("rust_rating_analysis_trend.png"),
        trend_chart,
    )?;

    let advantage = AdvantageData {
        radiant_gold: fixture.advantage.radiant_gold_adv,
        radiant_xp: fixture.advantage.radiant_xp_adv,
    };
    let advantage_chart = draw_advantage_graph(&advantage, Some(fixture.advantage.match_id))
        .ok_or("advantage fixture unexpectedly rendered no image")?
        .into_inner();
    fs::write(
        Path::new(&output_dir).join("rust_advantage.png"),
        advantage_chart,
    )?;

    // Exercise the same loader/HybridPetRenderer boundary used by
    // cama-runtime::pet_provider.  The checked-in component pack is passed
    // explicitly so this recording is hermetic and cannot silently use the
    // container-only /app path.
    let pet = fixture.pet;
    let components_path = resolve_fixture_path(Path::new(&fixture_path), &pet.components_path);
    let request = PetRenderRequest {
        species_id: &pet.species_id,
        stage: pet_stage(&pet.stage)?,
        mood: pet_mood(&pet.mood)?,
        seed: pet.seed,
        accessory: pet.accessory.as_deref(),
        evolution: None,
    };
    let assets_root = components_path
        .parent()
        .ok_or("pet components path has no parent")?;
    let mut pet_assets = PetAssetLoader::new(
        FilesystemPetAssets::new(assets_root),
        HybridPetRenderer::new(&components_path),
    );
    let pet_file = pet_assets.get_pet_card(&request);
    if pet_file.filename != pet.attachment_filename {
        return Err(format!(
            "unexpected pet attachment name: {} (expected {})",
            pet_file.filename, pet.attachment_filename
        )
        .into());
    }
    if pet.embed_image != format!("attachment://{}", pet_file.filename) {
        return Err(format!("unexpected pet attachment URL: {}", pet.embed_image).into());
    }
    fs::write(
        Path::new(&output_dir).join("rust_pet.png"),
        pet_file.bytes(),
    )?;

    let blame_luke = render_blame_luke(fixture.blame_luke.selected_index)?;
    fs::write(
        Path::new(&output_dir).join("rust_blame_luke.gif"),
        blame_luke.bytes,
    )?;

    if fixture.scout.portrait_mode != "cache_miss_fallback" {
        return Err("scout fixture must explicitly select cache_miss_fallback".into());
    }
    let scout = ScoutData {
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
    };
    let scout_png = NativeScoutImageRenderer::default().render(&ScoutReportInput {
        scout_data: scout,
        player_names: fixture.scout.player_names,
        title: fixture.scout.title,
    })?;
    fs::write(Path::new(&output_dir).join("rust_scout.png"), scout_png)?;

    // Exercise the same renderer called by the live Rust HeroGridCommandService
    // after its repository/source-resolution step.
    let hero_grid = &fixture.hero_grid;
    let stats = hero_grid
        .stats
        .iter()
        .map(|stat| HeroGridStat {
            discord_id: stat.discord_id,
            hero_id: stat.hero_id,
            games: stat.games,
            wins: stat.wins,
        })
        .collect::<Vec<_>>();
    let players = hero_grid
        .players
        .iter()
        .map(|player| HeroGridPlayer {
            discord_id: player.discord_id,
            name: player.name.clone(),
        })
        .collect::<Vec<_>>();
    let hero_grid_png =
        draw_hero_grid(&stats, &players, hero_grid.min_games, &hero_grid.title)?.into_inner();
    fs::write(
        Path::new(&output_dir).join("rust_hero_grid.png"),
        hero_grid_png,
    )?;

    // Exercise the same typed payload boundaries used by the live profile
    // Dota/Heroes tabs and `/matches recent`: role/lane distributions, hero
    // aggregate rows, and ordered match rows all feed the production drawing
    // helpers directly. This keeps the cross-runtime recording independent of
    // Discord and OpenDota while retaining the live renderer call shape.
    let profile = &fixture.profile;
    let role_graph = draw_role_graph(&profile.roles, &format!("Roles: {}", profile.username));
    fs::write(
        Path::new(&output_dir).join("rust_profile_role_graph.png"),
        role_graph.into_inner(),
    )?;
    let lanes = profile
        .lanes
        .iter()
        .map(|lane| (lane.name.clone(), lane.value))
        .collect::<Vec<_>>();
    fs::write(
        Path::new(&output_dir).join("rust_profile_lane_distribution.png"),
        draw_lane_distribution(&lanes).into_inner(),
    )?;
    let hero_stats = profile
        .hero_performance
        .iter()
        .map(|hero| HeroPerformanceEntry {
            hero_name: cama_app::hero_lookup::hero_name(hero.hero_id),
            games: hero.games,
            wins: hero.wins,
        })
        .collect::<Vec<_>>();
    fs::write(
        Path::new(&output_dir).join("rust_profile_hero_performance.png"),
        draw_hero_performance_chart(&hero_stats, &profile.username).into_inner(),
    )?;
    let recent_matches = profile
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
    fs::write(
        Path::new(&output_dir).join("rust_profile_recent_matches.png"),
        draw_matches_table(&recent_matches, &std::collections::BTreeMap::new()).into_inner(),
    )?;

    let animation = render_post_match_gif(
        &fixture.animation.name,
        fixture.animation.value,
        &fixture.animation.theme,
    )?;
    fs::write(
        Path::new(&output_dir).join("rust_animation.gif"),
        animation.bytes,
    )?;

    let terminal_crash = GifAsset::terminal_crash(
        &fixture.terminal_crash.name,
        fixture.terminal_crash.filing_number,
    );
    fs::write(
        Path::new(&output_dir).join("rust_terminal_crash.gif"),
        terminal_crash.bytes,
    )?;

    let pinnacle_source_path =
        resolve_fixture_path(Path::new(&fixture_path), &fixture.pinnacle.source_path);
    let pinnacle_source = fs::read(pinnacle_source_path)?;
    let request = RenderRequest::PinnaclePhase {
        boss_id: fixture.pinnacle.boss_id,
        phase: 3,
        secret: fixture.pinnacle.secret,
    };
    let pinnacle = NativeDigRenderer
        .render(&request, Some(&pinnacle_source))
        .map_err(|error| error.to_string())?;
    fs::write(
        Path::new(&output_dir).join("rust_pinnacle_phase3.gif"),
        pinnacle.bytes,
    )?;
    Ok(())
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
