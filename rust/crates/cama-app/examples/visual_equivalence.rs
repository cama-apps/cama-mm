//! Emit deterministic artifacts for the cross-language visual-equivalence gate.
//!
//! This is deliberately an example rather than a production path.  The
//! companion `scripts/visual_equivalence.py` owns the Python invocation and
//! image decoding; this binary only exercises the same public Rust renderers
//! that the runtime calls.

use std::env;
use std::fs;
use std::path::Path;

use cama_app::dig_assets::{DigRenderPort, RenderRequest};
use cama_app::dig_media_runtime::NativeDigRenderer;
use cama_app::drawing::{
    AdvantageData, BalancePoint, RatingHistoryEntry, draw_advantage_graph, draw_balance_chart,
    draw_prediction_market_chart, draw_rating_history_chart,
};
use cama_app::neon_degen::GifAsset;
use cama_app::post_match_gif_media::render_post_match_gif;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    chart: ChartFixture,
    animation: AnimationFixture,
    terminal_crash: TerminalCrashFixture,
    pinnacle: PinnacleFixture,
    balance: BalanceFixture,
    rating_history: RatingHistoryFixture,
    advantage: AdvantageFixture,
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
struct AdvantageFixture {
    match_id: i64,
    radiant_gold_adv: Vec<f64>,
    radiant_xp_adv: Vec<f64>,
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
